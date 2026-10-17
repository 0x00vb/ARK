//! An account's feed (ADR 0062, docs/AUTOMATIONS.md A2): the baseline, the cursor saved only
//! after the events before it, full and refused events, gaps with both recoveries, a lost
//! sign-in, rate limits and failures that may pass. A scripted source and no automations, so nothing polls on
//! its own and each poll is the test's.

use std::sync::Arc;

use ark_orchestrate::*;
use ark_store::Store;
use ark_tools::events::{
    Direction, EventSink, EventSource, ExternalEvent, FakeSource, SourceError,
};

struct Fixture {
    _dir: tempfile::TempDir,
    log: Arc<EventLog>,
    src: Arc<FakeSource>,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path()).unwrap());
    let log = EventLog::of(&store);
    let src = Arc::new(FakeSource::new("fake", "office@example.com"));
    log.provide(
        Offered {
            source: "fake".into(),
            account: "office@example.com".into(),
            label: "Inbox (office@example.com)".into(),
            actions: vec!["inbox.reply".into()],
        },
        src.clone() as Arc<dyn EventSource>,
    );
    Fixture {
        _dir: dir,
        log,
        src,
    }
}

impl Fixture {
    fn feed(&self) -> Arc<Feed> {
        self.log.feed("fake", "office@example.com").unwrap()
    }
    fn seen(&self) -> Vec<Option<String>> {
        self.src.seen.lock().unwrap().clone()
    }
}

fn mail(key: &str) -> ExternalEvent {
    ExternalEvent {
        source: "fake".into(),
        account: "office@example.com".into(),
        key: key.into(),
        kind: "message".into(),
        direction: Direction::Inbound,
        actor: Some("lara@example.com".into()),
        subject: Some(format!("thread-{key}")),
        at_ms: 1,
        label: "Mail from lara@example.com".into(),
        fields: Default::default(),
        payload: Some("the totals?".into()),
    }
}

#[tokio::test]
async fn the_first_poll_saves_a_baseline_and_later_events_are_saved_before_the_cursor() {
    let f = fixture();
    assert_eq!(f.feed().record().state, FeedState::Starting);
    f.src.push(Ok(vec![mail("old")]));
    let r = f.feed().poll_once().await.unwrap();
    assert_eq!(
        (r.state, r.cursor.as_deref()),
        (FeedState::Listening, Some("0"))
    );
    assert_eq!(f.log.current_seq(), 0, "the baseline runs nothing old");
    let r = f.feed().poll_once().await.unwrap();
    assert_eq!(r.cursor.as_deref(), Some("1"));
    assert_eq!(f.log.current_seq(), 1);
    assert_eq!(f.seen(), vec![None, Some("0".into())]);
    // Saved per store: another handle reads the same record.
    assert_eq!(
        f.log
            .feed_record("fake", "office@example.com")
            .unwrap()
            .cursor
            .as_deref(),
        Some("1")
    );
}

#[tokio::test]
async fn a_full_account_keeps_the_cursor_and_waits_before_polling_again() {
    let f = fixture();
    f.feed().poll_once().await.unwrap();
    f.log.set_limits(LogLimits {
        retained_bytes: 4,
        ..LogLimits::default()
    });
    f.src.push(Ok(vec![mail("m1")]));
    let r = f.feed().poll_once().await.unwrap();
    assert_eq!(r.cursor.as_deref(), Some("0"), "nothing saved, cursor kept");
    let FeedState::Degraded { error, retry_at_ms } = &r.state else {
        panic!("{:?}", r.state)
    };
    assert!(error.contains("full"), "{error}");
    assert!(*retry_at_ms > r.last_ok_ms.unwrap() + 29_000, "backs off");
    let polls = f.seen().len();
    f.feed().poll_once().await.unwrap();
    assert_eq!(f.seen().len(), polls, "no poll before the retry time");
}

#[tokio::test]
async fn an_event_that_breaks_the_contract_is_named_and_the_rest_are_kept() {
    let f = fixture();
    f.feed().poll_once().await.unwrap();
    let mut bad = mail("bad");
    bad.actor = Some("lara@example.com\nBcc: x".into());
    f.src.push(Ok(vec![mail("m1"), bad, mail("m2")]));
    let r = f.feed().poll_once().await.unwrap();
    assert_eq!(r.state, FeedState::Listening);
    assert_eq!(f.log.current_seq(), 2);
    assert_eq!(r.refused.len(), 1);
    assert!(
        r.refused[0].starts_with("bad: its actor has a line break"),
        "{:?}",
        r.refused
    );
}

#[tokio::test]
async fn a_gap_stops_the_feed_until_the_person_starts_from_now() {
    let f = fixture();
    let base = f.feed().poll_once().await.unwrap();
    f.src.push(Err(SourceError::Gap {
        since_ms: None,
        detail: "history expired".into(),
    }));
    let r = f.feed().poll_once().await.unwrap();
    assert_eq!(
        r.state,
        FeedState::Gap {
            since_ms: base.last_ok_ms,
            detail: "history expired".into()
        },
        "the gap starts at the last good poll"
    );
    let polls = f.seen().len();
    f.feed().poll_once().await.unwrap();
    assert_eq!(f.seen().len(), polls, "a gap polls nothing");
    let r = f.feed().recover(Recovery::StartFromNow).await.unwrap();
    assert_eq!(r.state, FeedState::Listening);
    assert_eq!(f.seen().last().unwrap(), &None, "a new baseline");
    assert_eq!(r.skipped.len(), 1);
    assert_eq!(r.skipped[0].since_ms, base.last_ok_ms);
    assert_eq!(r.skipped[0].detail, "history expired");
    assert!(
        f.feed().recover(Recovery::StartFromNow).await.is_err(),
        "no gap left"
    );
}

#[tokio::test]
async fn reconciling_a_gap_saves_what_the_provider_still_has_then_continues() {
    let f = fixture();
    let base = f.feed().poll_once().await.unwrap();
    f.src.push(Err(SourceError::Gap {
        since_ms: Some(42),
        detail: "history expired".into(),
    }));
    f.feed().poll_once().await.unwrap();
    // A reconcile that fails leaves the gap as it was.
    assert!(f.feed().recover(Recovery::Reconcile).await.is_err());
    assert!(matches!(f.feed().record().state, FeedState::Gap { .. }));
    f.src.reconcile_with(Ok(vec![mail("m1"), mail("m2")]));
    let r = f.feed().recover(Recovery::Reconcile).await.unwrap();
    assert_eq!(*f.src.reconciles.lock().unwrap(), vec![42, 42]);
    assert_eq!(r.state, FeedState::Listening);
    assert_eq!(f.log.current_seq(), 2);
    assert!(r.skipped.is_empty());
    assert_ne!(r.cursor, base.cursor);
}

#[tokio::test]
async fn a_lost_sign_in_needs_connection_and_a_new_one_resumes_from_the_saved_cursor() {
    let f = fixture();
    f.feed().poll_once().await.unwrap();
    f.src
        .push(Err(SourceError::Disconnected("token revoked".into())));
    let r = f.feed().poll_once().await.unwrap();
    assert_eq!(
        r.state,
        FeedState::NeedsConnection {
            detail: "token revoked".into()
        }
    );
    f.src.push(Ok(vec![mail("m1")]));
    let r = f.feed().poll_once().await.unwrap();
    assert_eq!(r.state, FeedState::Listening);
    assert_eq!(
        f.seen().last().unwrap().as_deref(),
        Some("0"),
        "from the saved cursor"
    );
    assert_eq!(f.log.current_seq(), 1);
}

#[tokio::test]
async fn a_rate_limit_and_a_failure_that_may_pass_wait_before_polling_again() {
    let f = fixture();
    f.feed().poll_once().await.unwrap();
    f.src.push(Err(SourceError::RateLimited {
        retry_at_ms: u64::MAX / 2,
    }));
    let r = f.feed().poll_once().await.unwrap();
    assert_eq!(
        r.state,
        FeedState::RateLimited {
            until_ms: u64::MAX / 2
        }
    );
    let polls = f.seen().len();
    f.feed().poll_once().await.unwrap();
    assert_eq!(f.seen().len(), polls);

    let g = fixture();
    g.feed().poll_once().await.unwrap();
    g.src.push(Err(SourceError::Transient("503".into())));
    let first = g.feed().poll_once().await.unwrap();
    let FeedState::Degraded { retry_at_ms: a, .. } = first.state else {
        panic!()
    };
    assert_eq!(first.failures, 1);
    assert!(a > first.last_ok_ms.unwrap(), "waits before trying again");
}

#[tokio::test]
async fn events_saved_before_a_crash_that_lost_the_cursor_are_kept_once_when_polled_again() {
    let f = fixture();
    f.feed().poll_once().await.unwrap();
    // The process saved the batch, then stopped before the cursor was saved.
    f.log.ingest(vec![mail("m1"), mail("m2")]).unwrap();
    assert_eq!(f.feed().record().cursor.as_deref(), Some("0"));
    // After the restart the provider lists the same events again from the old cursor.
    f.src.push(Ok(vec![mail("m1"), mail("m2"), mail("m3")]));
    let r = f.feed().poll_once().await.unwrap();
    assert_eq!(r.cursor.as_deref(), Some("1"));
    assert_eq!(f.log.current_seq(), 3, "m1 and m2 once each, m3 new");
}

#[tokio::test]
async fn a_disconnected_account_needs_connection_and_resumes_from_its_cursor_when_back() {
    let f = fixture();
    let offered = Offered {
        source: "fake".into(),
        account: "office@example.com".into(),
        label: "Inbox (office@example.com)".into(),
        actions: vec![],
    };
    f.feed().poll_once().await.unwrap();
    f.log.sync(vec![]);
    assert!(f.log.feed("fake", "office@example.com").is_none());
    assert_eq!(
        f.log
            .feed_record("fake", "office@example.com")
            .unwrap()
            .state,
        FeedState::NeedsConnection {
            detail: "Inbox (office@example.com) was disconnected".into()
        }
    );
    // Connected again after the start: a new source, polled from the saved cursor.
    let again = Arc::new(FakeSource::new("fake", "office@example.com"));
    again.push(Ok(vec![mail("m1")]));
    f.log.sync(vec![(
        offered.clone(),
        Some(again.clone() as Arc<dyn EventSource>),
    )]);
    let r = f.feed().poll_once().await.unwrap();
    assert_eq!(r.state, FeedState::Listening);
    assert_eq!(again.seen.lock().unwrap().clone(), vec![Some("0".into())]);
    assert_eq!(f.log.current_seq(), 1);
    // Syncing the same account again keeps the feed it has.
    let other = Arc::new(FakeSource::new("fake", "office@example.com"));
    f.log
        .sync(vec![(offered, Some(other.clone() as Arc<dyn EventSource>))]);
    f.feed().poll_once().await.unwrap();
    assert!(other.seen.lock().unwrap().is_empty());
}
