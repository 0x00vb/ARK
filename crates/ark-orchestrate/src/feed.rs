//! One poller per connected account (ADR 0062, `docs/AUTOMATIONS.md` › Mail): a [`Feed`] polls
//! an [`EventSource`], saves what it got in the [`EventLog`], and only then keeps the
//! provider's cursor. Its [`FeedRecord`] is what every automation on the account shows.
//!
//! | Rule | Why |
//! |---|---|
//! | The cursor is saved after the events before it | a crash between the two delivers them again, and the log keeps each once |
//! | No cursor yet: the provider's present becomes the baseline | old mail never runs |
//! | A gap stops the feed until the person reconciles or starts from now | ARK never jumps over events it cannot prove it saw |
//! | Starting from now keeps the skipped interval | what may be missing stays visible |
//! | An event that breaks the event contract is let go and named; the rest are kept | one bad message does not stop an account |

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use ark_tools::events::{EventSink, EventSource, ExternalEvent, IngestError, SourceError};
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

use crate::automation::EventLog;
use crate::record::now_ms;

pub(crate) const FEED: &str = "event_feed";

/// Events asked of a provider per poll.
const MAX: usize = 100;
/// Polls in one round while the provider says it has more.
const ROUNDS: usize = 20;
/// The first wait after a failure that may pass; it doubles up to [`BACKOFF_MAX`].
const BACKOFF: Duration = Duration::from_secs(30);
const BACKOFF_MAX: Duration = Duration::from_secs(15 * 60);
/// Refused events named in the record, most recent last.
const REFUSED_KEPT: usize = 20;

/// How an account's feed stands.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum FeedState {
    /// No cursor yet: the provider's present is being saved as the baseline.
    Starting,
    Listening,
    /// Polls fail for a reason that may pass; the next try is at `retry_at_ms`.
    Degraded {
        error: String,
        retry_at_ms: u64,
    },
    /// The account is not connected any more. Polls go on, so a new sign-in resumes it from
    /// the saved cursor.
    NeedsConnection {
        detail: String,
    },
    /// The provider cannot list changes since the cursor: events since `since_ms` may be
    /// missing. Nothing is read until the person chooses a [`Recovery`].
    Gap {
        since_ms: Option<u64>,
        detail: String,
    },
    /// The provider asked to wait until then.
    RateLimited {
        until_ms: u64,
    },
}

/// An interval the person chose to start from now over: events in it may be missing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkippedGap {
    pub since_ms: Option<u64>,
    pub until_ms: u64,
    pub detail: String,
}

/// One account's feed, saved per store.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedRecord {
    pub source: String,
    pub account: String,
    /// Where the next poll starts. `None` until the baseline is saved.
    pub cursor: Option<String>,
    pub state: FeedState,
    /// The last poll that saved what it got.
    pub last_ok_ms: Option<u64>,
    /// Failures in a row that may pass.
    #[serde(default)]
    pub failures: u32,
    #[serde(default)]
    pub skipped: Vec<SkippedGap>,
    /// Events the provider gave that broke the event contract and were let go: `key: why`.
    #[serde(default)]
    pub refused: Vec<String>,
}

impl FeedRecord {
    fn new(source: &str, account: &str) -> Self {
        FeedRecord {
            source: source.into(),
            account: account.into(),
            cursor: None,
            state: FeedState::Starting,
            last_ok_ms: None,
            failures: 0,
            skipped: Vec::new(),
            refused: Vec::new(),
        }
    }
}

/// The two ways out of a [`FeedState::Gap`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recovery {
    /// List what the provider still has since the gap began, save it, then continue.
    Reconcile,
    /// Continue from the provider's present and keep the gap as skipped.
    StartFromNow,
}

/// The poller of one account. [`EventLog::provide`] starts at most one per account.
pub struct Feed {
    log: Weak<EventLog>,
    source: Arc<dyn EventSource>,
    every: Duration,
    /// One poll or recovery at a time.
    step: tokio::sync::Mutex<()>,
    wake: Notify,
    stopped: AtomicBool,
}

impl Feed {
    pub(crate) fn new(log: &Arc<EventLog>, source: Arc<dyn EventSource>, every: Duration) -> Self {
        Feed {
            log: Arc::downgrade(log),
            source,
            every,
            step: tokio::sync::Mutex::new(()),
            wake: Notify::new(),
            stopped: AtomicBool::new(false),
        }
    }

    pub fn source(&self) -> &str {
        self.source.source()
    }

    pub fn account(&self) -> &str {
        self.source.account()
    }

    /// The saved record, or a fresh one before the first poll.
    pub fn record(&self) -> FeedRecord {
        self.log
            .upgrade()
            .and_then(|l| l.feed_record(self.source(), self.account()))
            .unwrap_or_else(|| FeedRecord::new(self.source(), self.account()))
    }

    /// Poll until the provider has nothing more (at most a few rounds), saving each batch
    /// before its cursor. A gap, or a wait the provider or a failure set, polls nothing.
    pub async fn poll_once(&self) -> Result<FeedRecord, String> {
        let _one = self.step.lock().await;
        let log = self.log.upgrade().ok_or("the store is closed")?;
        let mut r = self.record();
        let now = now_ms();
        match &r.state {
            FeedState::Gap { .. } => return Ok(r),
            FeedState::Degraded { retry_at_ms, .. } if *retry_at_ms > now => return Ok(r),
            FeedState::RateLimited { until_ms } if *until_ms > now => return Ok(r),
            _ => {}
        }
        for _ in 0..ROUNDS {
            match self.source.poll(r.cursor.as_deref(), MAX).await {
                Ok(b) => {
                    if let Err(e) = keep(&log, &mut r, b.events) {
                        r = degraded(r, e.to_string(), self.every);
                        break;
                    }
                    r.cursor = Some(b.cursor);
                    r.state = FeedState::Listening;
                    r.failures = 0;
                    r.last_ok_ms = Some(now_ms());
                    log.save_feed(&r)?;
                    if !b.more {
                        break;
                    }
                }
                Err(e) => {
                    r = failed(r, e, self.every);
                    break;
                }
            }
        }
        log.save_feed(&r)?;
        Ok(r)
    }

    /// Leave a gap the way the person chose. Anything else than a gap is refused; a failed
    /// recovery leaves the gap as it was.
    pub async fn recover(&self, how: Recovery) -> Result<FeedRecord, String> {
        let _one = self.step.lock().await;
        let log = self.log.upgrade().ok_or("the store is closed")?;
        let mut r = self.record();
        let FeedState::Gap { since_ms, detail } = r.state.clone() else {
            return Err(format!(
                "{} on {} has no gap to recover from",
                r.account, r.source
            ));
        };
        match how {
            Recovery::Reconcile => {
                let since = since_ms.ok_or(
                    "when the gap began is not known, so it cannot be checked; start from now",
                )?;
                let b = self
                    .source
                    .reconcile(since, MAX)
                    .await
                    .map_err(|e| e.to_string())?;
                keep(&log, &mut r, b.events).map_err(|e| e.to_string())?;
                r.cursor = Some(b.cursor);
            }
            Recovery::StartFromNow => {
                let b = self
                    .source
                    .poll(None, MAX)
                    .await
                    .map_err(|e| e.to_string())?;
                r.cursor = Some(b.cursor);
                r.skipped.push(SkippedGap {
                    since_ms,
                    until_ms: now_ms(),
                    detail,
                });
            }
        }
        r.state = FeedState::Listening;
        r.failures = 0;
        r.last_ok_ms = Some(now_ms());
        log.save_feed(&r)?;
        self.wake.notify_one();
        Ok(r)
    }

    /// How long to wait before the next poll after `r`.
    fn wait_after(&self, r: &FeedRecord) -> Duration {
        let now = now_ms();
        let until = |t: u64| Duration::from_millis(t.saturating_sub(now)).max(self.every);
        match &r.state {
            FeedState::Degraded { retry_at_ms, .. } => until(*retry_at_ms),
            FeedState::RateLimited { until_ms } => until(*until_ms),
            _ => self.every,
        }
    }

    pub(crate) fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        self.wake.notify_one();
    }

    /// Poll until stopped. A recovery wakes it at once.
    pub(crate) async fn run(self: Arc<Self>) {
        while !self.stopped.load(Ordering::SeqCst) {
            let wait = match self.poll_once().await {
                Ok(r) => self.wait_after(&r),
                Err(e) => {
                    tracing::warn!(
                        source = self.source(),
                        account = self.account(),
                        "feed: {e}"
                    );
                    self.every
                }
            };
            tokio::select! {
                _ = tokio::time::sleep(wait) => {}
                _ = self.wake.notified() => {}
            }
        }
    }
}

/// Save `events`. A batch that breaks the event contract is saved one by one, letting go of
/// the events that break it and naming them in the record.
fn keep(log: &EventLog, r: &mut FeedRecord, events: Vec<ExternalEvent>) -> Result<(), IngestError> {
    if events.is_empty() {
        return Ok(());
    }
    match log.ingest(events.clone()) {
        Ok(_) => Ok(()),
        Err(IngestError::Invalid(_)) => {
            for e in events {
                let key = e.key.clone();
                match log.ingest(vec![e]) {
                    Ok(_) => {}
                    Err(IngestError::Invalid(why)) => {
                        r.refused.push(format!("{key}: {why}"));
                        let over = r.refused.len().saturating_sub(REFUSED_KEPT);
                        r.refused.drain(..over);
                    }
                    Err(e) => return Err(e),
                }
            }
            Ok(())
        }
        Err(e) => Err(e),
    }
}

fn degraded(mut r: FeedRecord, error: String, every: Duration) -> FeedRecord {
    r.failures += 1;
    let wait = BACKOFF
        .max(every)
        .saturating_mul(1 << (r.failures - 1).min(10))
        .min(BACKOFF_MAX);
    r.state = FeedState::Degraded {
        error,
        retry_at_ms: now_ms() + wait.as_millis() as u64,
    };
    r
}

fn failed(mut r: FeedRecord, e: SourceError, every: Duration) -> FeedRecord {
    match e {
        SourceError::Disconnected(detail) => r.state = FeedState::NeedsConnection { detail },
        SourceError::Gap { since_ms, detail } => {
            r.state = FeedState::Gap {
                since_ms: since_ms.or(r.last_ok_ms),
                detail,
            }
        }
        SourceError::RateLimited { retry_at_ms } => {
            r.state = FeedState::RateLimited {
                until_ms: retry_at_ms,
            }
        }
        SourceError::Transient(error) => return degraded(r, error, every),
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failures_that_may_pass_wait_twice_as_long_each_time_up_to_a_bound() {
        let every = Duration::from_secs(60);
        let mut r = FeedRecord::new("fake", "a");
        let mut waits = Vec::new();
        for _ in 0..8 {
            let before = now_ms();
            r = degraded(r, "503".into(), every);
            let FeedState::Degraded { retry_at_ms, .. } = r.state else {
                panic!()
            };
            waits.push((retry_at_ms - before) / 1000);
        }
        assert_eq!(&waits[..5], &[60, 120, 240, 480, 900]);
        assert!(waits[5..].iter().all(|w| *w == 900), "{waits:?}");
        let ok = failed(
            r,
            SourceError::Gap {
                since_ms: None,
                detail: "x".into(),
            },
            every,
        );
        assert_eq!(
            ok.state,
            FeedState::Gap {
                since_ms: None,
                detail: "x".into()
            }
        );
    }
}
