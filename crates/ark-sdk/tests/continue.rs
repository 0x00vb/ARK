//! ADR 0048: a chat turn that did not finish is continued by the next one, whatever stopped it
//! (the app stopping, the person, a crash: see `crash_recovery.rs`), without making its changes
//! twice. In process: the stop is `Engine::stop_soon` or a cancel.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ark_sdk::prelude::ark_model::{Completion, MockProvider, ToolCall, Usage};
use ark_sdk::prelude::ark_runtime::session::INTERRUPTED;
use ark_sdk::prelude::*;
use serde_json::json;
use tokio::sync::Notify;

/// `t.write` counts its runs; `t.wait` says it started and then waits for a stop.
struct Tools {
    writes: AtomicUsize,
    started: Notify,
}

#[async_trait::async_trait]
impl Adapter for Tools {
    fn name(&self) -> &str {
        "t"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![
            ActionDescriptor::new("t.write", "a change", RiskTier::Mutate),
            ActionDescriptor::new("t.wait", "a long read", RiskTier::Read),
        ]
    }
    async fn run(
        &self,
        action: &str,
        _i: serde_json::Value,
        _c: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        if action == "t.write" {
            self.writes.fetch_add(1, Ordering::SeqCst);
        } else {
            self.started.notify_one();
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
        Ok(ActionOutput::json(json!({ "ok": true })))
    }
}

fn tools() -> Arc<Tools> {
    Arc::new(Tools {
        writes: AtomicUsize::new(0),
        started: Notify::new(),
    })
}

fn ark(dir: &std::path::Path, tools: &Arc<Tools>, script: Vec<Completion>) -> Ark {
    Ark::builder()
        .store_root(dir)
        .allow_mutate(true)
        .tool(tools.clone())
        .profile(
            AgentProfile::builder()
                .core_actions(["t.write", "t.wait"])
                .build(),
        )
        .provider(Arc::new(MockProvider::new("m", script)) as Arc<dyn ark_model::Provider>)
        .planner(false)
        .critic(false)
        .build()
        .unwrap()
}

fn step(name: &str) -> Completion {
    Completion {
        content: String::new(),
        tool_calls: vec![ToolCall {
            id: format!("c-{name}"),
            name: name.into(),
            arguments: json!({}),
        }],
        usage: Usage::default(),
    }
}

fn say(text: &str) -> Completion {
    Completion {
        content: text.into(),
        tool_calls: vec![],
        usage: Usage::default(),
    }
}

/// A turn that writes, then waits until `stop` runs.
async fn stopped_turn(
    ark: &Ark,
    tools: &Arc<Tools>,
    session: &mut Session,
    cancel: Option<CancelToken>,
    stop: impl FnOnce(),
) -> (HarnessOutcome, Vec<HarnessEvent>) {
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = events.clone();
    let turn = ark.ask_session(
        session,
        "write, then wait",
        TurnOptions {
            cancel,
            ..Default::default()
        },
        move |e| sink.lock().unwrap().push(e),
    );
    tokio::pin!(turn);
    tokio::select! {
        _ = &mut turn => panic!("the turn ended before the stop"),
        _ = tools.started.notified() => stop(),
    }
    let out = turn.await.unwrap();
    let events = events.lock().unwrap().clone();
    (out, events)
}

#[tokio::test]
async fn a_turn_the_app_stops_is_interrupted_not_stopped_and_continues_after() {
    let dir = tempfile::tempdir().unwrap();
    let t = tools();
    let first = ark(dir.path(), &t, vec![step("t.write"), step("t.wait")]);
    let p = first.find_or_create_project("p", "").unwrap();
    let mut session = first.start_session(&p.id).unwrap();
    let engine = first.shared_engine();
    let (out, events) = stopped_turn(&first, &t, &mut session, None, || engine.stop_soon()).await;
    assert!(out.cancelled);
    assert!(events
        .iter()
        .any(|e| matches!(e, HarnessEvent::Interrupted)));
    assert!(!events.iter().any(|e| matches!(e, HarnessEvent::Cancelled)));
    let turns = session.transcript(first.engine(), None).unwrap();
    assert!(turns[0].incomplete && !turns[0].cancelled);
    assert_eq!(turns[0].error.as_deref(), Some(INTERRUPTED));
    drop(session);
    drop(first);

    // The next process.
    let next = ark(dir.path(), &t, vec![step("t.write"), say("Done.")]);
    let mut session = next.latest_session(&p.id).unwrap().unwrap();
    let out = next
        .ask_session(&mut session, "continue", TurnOptions::default(), |_| {})
        .await
        .unwrap();
    assert_eq!(out.answer, "Done.");
    assert_eq!(t.writes.load(Ordering::SeqCst), 1, "written once");
}

#[tokio::test]
async fn a_turn_the_person_stopped_is_stopped_and_its_change_is_not_made_again() {
    let dir = tempfile::tempdir().unwrap();
    let t = tools();
    let a = ark(
        dir.path(),
        &t,
        vec![
            step("t.write"),
            step("t.wait"),
            step("t.write"),
            say("Done."),
        ],
    );
    let p = a.find_or_create_project("p", "").unwrap();
    let mut session = a.start_session(&p.id).unwrap();
    let cancel = CancelToken::new();
    let c = cancel.clone();
    let (out, events) = stopped_turn(&a, &t, &mut session, Some(cancel), move || c.cancel()).await;
    assert!(out.cancelled);
    assert!(events.iter().any(|e| matches!(e, HarnessEvent::Cancelled)));
    let turns = session.transcript(a.engine(), None).unwrap();
    assert!(turns[0].cancelled && !turns[0].incomplete);

    a.ask_session(&mut session, "go on", TurnOptions::default(), |_| {})
        .await
        .unwrap();
    assert_eq!(t.writes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn another_chat_in_the_project_does_not_continue_this_one() {
    let dir = tempfile::tempdir().unwrap();
    let t = tools();
    let a = ark(
        dir.path(),
        &t,
        vec![
            step("t.write"),
            step("t.wait"),
            step("t.write"),
            say("Done."),
        ],
    );
    let p = a.find_or_create_project("p", "").unwrap();
    let mut one = a.start_session(&p.id).unwrap();
    let cancel = CancelToken::new();
    let c = cancel.clone();
    stopped_turn(&a, &t, &mut one, Some(cancel), move || c.cancel()).await;

    // A second chat asks for the same change: it is its own work and runs.
    let mut two = a.start_session(&p.id).unwrap();
    assert!(two.unfinished(a.engine()).unwrap().is_none());
    a.ask_session(&mut two, "write", TurnOptions::default(), |_| {})
        .await
        .unwrap();
    assert_eq!(t.writes.load(Ordering::SeqCst), 2);
    let one_calls = one.unfinished(a.engine()).unwrap().unwrap().calls;
    assert_eq!(
        one_calls.len(),
        2,
        "only the first chat's calls: {one_calls:#?}"
    );
}

#[tokio::test]
async fn a_turn_stored_in_full_before_the_process_stopped_is_shown_finished() {
    let dir = tempfile::tempdir().unwrap();
    let t = tools();
    let a = ark(dir.path(), &t, vec![say("hello")]);
    let p = a.find_or_create_project("p", "").unwrap();
    let session = a.start_session(&p.id).unwrap();
    let id = session.id().as_str().to_string();
    // The turn row was stored; the process stopped before the session's header counted it.
    let finished = json!({
        "index": 0, "input": "hi", "answer": "hello", "cited": [], "cancelled": false,
        "steered": [], "at": "2026-10-07T10:00:00Z"
    });
    a.engine()
        .store
        .put_session_turn(&id, 0, &finished)
        .unwrap();
    // And a failed one, stored with its error.
    drop(session);
    let session = a.open_session(&id).unwrap().unwrap();
    let turns = session.transcript(a.engine(), None).unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].answer, "hello");
    assert!(!turns[0].incomplete);

    let failed = json!({
        "index": 1, "input": "again", "answer": "", "cancelled": false, "steered": [],
        "at": "2026-10-07T10:01:00Z", "incomplete": true, "error": "the model refused"
    });
    a.engine().store.put_session_turn(&id, 1, &failed).unwrap();
    let session = a.open_session(&id).unwrap().unwrap();
    let turns = session.transcript(a.engine(), None).unwrap();
    assert_eq!(
        turns[1].error.as_deref(),
        Some("the model refused"),
        "its own error kept"
    );
}
