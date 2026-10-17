//! Events from outside (a mail arrived, a WhatsApp Business message came in) as provider-neutral
//! values, and the contracts between the code that receives them and the code that keeps them
//! (ADR 0062). A provider authenticates its transport and normalizes each event into an
//! [`ExternalEvent`]; an [`EventSink`] saves it before it answers. Polled providers implement
//! [`EventSource`]; pushed ones implement [`WebhookSource`]. Nothing here stores anything.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Which way a message went, from the connected account's side.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Inbound,
    Outbound,
}

/// One normalized event from a transport-authenticated provider.
///
/// `actor` is the provider's claim of who did it (a canonical mail address, a WhatsApp id):
/// it says the transport delivered it, not that a person was proved to be behind it. `payload`
/// is content from outside (a message body): data for a run, never instructions to ARK.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalEvent {
    /// The provider family (`gmail`, `whatsapp`).
    pub source: String,
    /// The connected account the event belongs to.
    pub account: String,
    /// Opaque and stable per semantic event: a re-delivery keeps it, another transition (a
    /// status change of the same message) gets another.
    pub key: String,
    /// What happened (`message`, `status`).
    pub kind: String,
    pub direction: Direction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    /// The thread or conversation it belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// When the provider says it happened (Unix ms).
    pub at_ms: u64,
    /// One line about the event in trusted words, written by the source from checked fields
    /// (`Mail from lara@example.com`). Shown to the person and put before the payload in a run.
    #[serde(default)]
    pub label: String,
    /// Small scalar values a run or an authority may bind to (`message_id`, `reply_until_ms`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fields: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<String>,
}

/// The size limits of events. [`Limits::default`] is the documented contract
/// (`docs/AUTOMATIONS.md` › Records and one-writer rule); tests use tighter ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// A webhook body, raw, before it is verified.
    pub raw_body: usize,
    /// One event's payload.
    pub payload: usize,
    /// Scalar fields per event.
    pub fields: usize,
    /// Bytes of one field's name and value together.
    pub field: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            raw_body: 1 << 20,
            payload: 256 << 10,
            fields: 64,
            field: 4 << 10,
        }
    }
}

impl ExternalEvent {
    /// Refuse an event over `limits` or without the identity every event needs.
    pub fn check(&self, limits: &Limits) -> Result<(), IngestError> {
        let bad = |why: String| Err(IngestError::Invalid(why));
        for (name, v) in [
            ("source", &self.source),
            ("account", &self.account),
            ("key", &self.key),
            ("kind", &self.kind),
        ] {
            if v.trim().is_empty() {
                return bad(format!("an event needs a {name}"));
            }
            if v.len() > limits.field {
                return bad(format!("its {name} is longer than {} bytes", limits.field));
            }
        }
        if self.fields.len() > limits.fields {
            return bad(format!(
                "{} fields; at most {} are kept",
                self.fields.len(),
                limits.fields
            ));
        }
        for (k, v) in &self.fields {
            if k.len() + v.len() > limits.field {
                return bad(format!("field `{k}` is longer than {} bytes", limits.field));
            }
        }
        for (name, v) in [("actor", &self.actor), ("subject", &self.subject)] {
            if v.as_ref().is_some_and(|v| v.len() > limits.field) {
                return bad(format!("its {name} is longer than {} bytes", limits.field));
            }
        }
        if self.label.len() > limits.field {
            return bad(format!("its label is longer than {} bytes", limits.field));
        }
        if let Some(p) = &self.payload {
            if p.len() > limits.payload {
                return bad(format!(
                    "a payload of {} bytes; at most {} are kept",
                    p.len(),
                    limits.payload
                ));
            }
        }
        Ok(())
    }
}

/// Why events were not saved.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum IngestError {
    /// No room now: nothing of the batch was saved and it may be offered again later. A pull
    /// feed keeps its cursor; a webhook answers with a retryable failure.
    #[error("full: {0}")]
    Full(String),
    /// The batch breaks the event contract; offering it again does not help.
    #[error("{0}")]
    Invalid(String),
    /// The store failed; nothing of the batch was saved and it may be offered again later.
    #[error("store: {0}")]
    Store(String),
}

impl IngestError {
    /// Offering the same batch later may succeed.
    pub fn retryable(&self) -> bool {
        !matches!(self, IngestError::Invalid(_))
    }
}

/// What a sink did with a batch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ingested {
    /// Saved for the first time.
    pub new: usize,
    /// Already saved: a re-delivery, left as it was.
    pub repeated: usize,
}

/// Where events go: saved before `ingest` returns, so whoever delivered them may acknowledge.
/// A batch is saved whole or not at all; delivering the same events again is harmless.
pub trait EventSink: Send + Sync {
    fn ingest(&self, batch: Vec<ExternalEvent>) -> Result<Ingested, IngestError>;
}

/// Events a [`EventSource::poll`] got, and where the next poll starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Batch {
    pub events: Vec<ExternalEvent>,
    /// The cursor after exactly these events. The caller keeps it only once they are saved.
    pub cursor: String,
    /// The provider has more after `cursor` (the batch stopped at `max`).
    pub more: bool,
}

/// Why a poll got nothing.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SourceError {
    /// The account is not connected any more (signed out, revoked).
    #[error("needs connection: {0}")]
    Disconnected(String),
    /// The provider cannot list changes from the cursor (it expired): events in between may be
    /// lost. Nothing moves on until the person chooses how to recover.
    #[error("recovery gap: {0}")]
    Gap(String),
    /// The provider asked to wait until then (Unix ms).
    #[error("rate limited until {retry_at_ms}")]
    RateLimited { retry_at_ms: u64 },
    /// Anything that may pass on its own (network, a provider error).
    #[error("{0}")]
    Transient(String),
}

/// A provider polled from this machine (no public address needed). Cursors stay with the
/// caller, which saves one only after the events before it are saved.
#[async_trait]
pub trait EventSource: Send + Sync {
    fn source(&self) -> &str;
    fn account(&self) -> &str;
    /// `None`: the baseline, a cursor at the provider's present with no events, so nothing old
    /// runs. `Some(c)`: the events after `c`, at most `max`, paged to the end of whole pages.
    async fn poll(&self, cursor: Option<&str>, max: usize) -> Result<Batch, SourceError>;
}

/// Why a webhook request was refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum HookError {
    /// The signature or the verification token does not match: not from the provider.
    #[error("not verified: {0}")]
    Unverified(String),
    /// Verified, but for another account than the one connected.
    #[error("another account: {0}")]
    Unbound(String),
    /// Verified, but not an event body this source reads.
    #[error("malformed: {0}")]
    Malformed(String),
}

/// A provider that pushes events to a public HTTPS route the person supplies.
pub trait WebhookSource: Send + Sync {
    fn source(&self) -> &str;
    fn account(&self) -> &str;
    /// Answer the provider's subscription check (a GET with its query); the body to send back.
    fn challenge(&self, query: &BTreeMap<String, String>) -> Result<String, HookError>;
    /// Verify the raw body against its headers, then decode it. Verification comes first:
    /// nothing is parsed from a body that is not proved to come from the provider.
    fn decode(
        &self,
        headers: &BTreeMap<String, String>,
        raw: &[u8],
    ) -> Result<Vec<ExternalEvent>, HookError>;
}

/// A scripted [`EventSource`] for tests and demos: each poll takes the next step. A batch step
/// returns its events and the cursor `n` (the count of steps taken); an empty script returns
/// no events at the same cursor.
pub struct FakeSource {
    source: String,
    account: String,
    steps: Mutex<VecDeque<Result<Vec<ExternalEvent>, SourceError>>>,
    taken: Mutex<u64>,
    /// The cursors each poll was given, in order.
    pub seen: Mutex<Vec<Option<String>>>,
}

impl FakeSource {
    pub fn new(source: &str, account: &str) -> Self {
        FakeSource {
            source: source.into(),
            account: account.into(),
            steps: Mutex::new(VecDeque::new()),
            taken: Mutex::new(0),
            seen: Mutex::new(Vec::new()),
        }
    }
    pub fn push(&self, step: Result<Vec<ExternalEvent>, SourceError>) {
        self.steps.lock().unwrap().push_back(step);
    }
}

#[async_trait]
impl EventSource for FakeSource {
    fn source(&self) -> &str {
        &self.source
    }
    fn account(&self) -> &str {
        &self.account
    }
    async fn poll(&self, cursor: Option<&str>, max: usize) -> Result<Batch, SourceError> {
        self.seen.lock().unwrap().push(cursor.map(str::to_string));
        let mut taken = self.taken.lock().unwrap();
        if cursor.is_none() {
            return Ok(Batch {
                events: Vec::new(),
                cursor: taken.to_string(),
                more: false,
            });
        }
        let mut steps = self.steps.lock().unwrap();
        match steps.pop_front() {
            None => Ok(Batch {
                events: Vec::new(),
                cursor: taken.to_string(),
                more: false,
            }),
            Some(Err(e)) => Err(e),
            Some(Ok(mut events)) => {
                if events.len() > max {
                    // Not taken: offered again whole next time, as a provider pages.
                    let rest = events.split_off(max);
                    let mut again = events.clone();
                    again.extend(rest);
                    steps.push_front(Ok(again));
                    return Ok(Batch {
                        events: Vec::new(),
                        cursor: taken.to_string(),
                        more: true,
                    });
                }
                *taken += 1;
                Ok(Batch {
                    events,
                    cursor: taken.to_string(),
                    more: !steps.is_empty(),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(key: &str) -> ExternalEvent {
        ExternalEvent {
            source: "fake".into(),
            account: "a".into(),
            key: key.into(),
            kind: "message".into(),
            direction: Direction::Inbound,
            actor: Some("lara@example.com".into()),
            subject: None,
            at_ms: 1,
            label: "Message from lara@example.com".into(),
            fields: BTreeMap::new(),
            payload: Some("hello".into()),
        }
    }

    #[test]
    fn events_over_the_limits_are_refused() {
        let tight = Limits {
            raw_body: 64,
            payload: 4,
            fields: 1,
            field: 40,
        };
        assert!(event("k").check(&Limits::default()).is_ok());
        let e = event("k").check(&tight).unwrap_err();
        assert!(e.to_string().contains("payload of 5 bytes"), "{e}");
        assert!(!e.retryable());
        let mut many = event("k");
        many.payload = None;
        many.fields.insert("a".into(), "1".into());
        many.fields.insert("b".into(), "2".into());
        assert!(many.check(&tight).is_err());
        let mut long = event("k");
        long.payload = None;
        long.fields.insert("a".into(), "x".repeat(40));
        assert!(long.check(&tight).is_err());
        let mut nokey = event(" ");
        nokey.payload = None;
        assert!(nokey.check(&tight).is_err());
        assert!(IngestError::Full("x".into()).retryable());
    }

    #[tokio::test]
    async fn the_fake_source_starts_at_a_baseline_and_pages_whole_batches() {
        let s = FakeSource::new("fake", "a");
        s.push(Ok(vec![event("1"), event("2")]));
        s.push(Err(SourceError::Transient("down".into())));
        let base = s.poll(None, 10).await.unwrap();
        assert!(base.events.is_empty());
        let small = s.poll(Some(&base.cursor), 1).await.unwrap();
        assert!(
            small.events.is_empty() && small.more,
            "too big: offered again"
        );
        let b = s.poll(Some(&base.cursor), 10).await.unwrap();
        assert_eq!(b.events.len(), 2);
        assert_eq!(b.cursor, "1");
        assert!(s.poll(Some(&b.cursor), 10).await.is_err());
        assert!(s.poll(Some(&b.cursor), 10).await.unwrap().events.is_empty());
    }
}
