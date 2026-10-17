//! Event automations A0 (ADR 0062, docs/AUTOMATIONS.md): one run per event across re-delivery
//! and restarts, activation cutovers (event before create, pause, resume, change, stop), Full
//! then drain or discard, crash points between every durable step, the account inbox limit,
//! and event content kept out of other runs. Scripted models and a fake source; no network.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ark_model::{Completion, Message, Provider, ProviderError, ToolDef, Usage};
use ark_orchestrate::*;
use ark_sdk::prelude::*;
use ark_tools::events::{Direction, EventSink, ExternalEvent, IngestError};
use serde_json::json;
use tokio::sync::Semaphore;

/// A scripted model: each call takes the next reply and records the request it saw. With a
/// gate, each call waits for one permit first.
struct Script {
    replies: Mutex<VecDeque<Result<Completion, ProviderError>>>,
    seen: Mutex<Vec<String>>,
    gate: Option<Arc<Semaphore>>,
}

impl Script {
    fn new(replies: Vec<Result<Completion, ProviderError>>) -> Arc<Self> {
        Self::gated(replies, None)
    }
    fn gated(
        replies: Vec<Result<Completion, ProviderError>>,
        gate: Option<Arc<Semaphore>>,
    ) -> Arc<Self> {
        Arc::new(Script {
            replies: Mutex::new(replies.into()),
            seen: Mutex::new(Vec::new()),
            gate,
        })
    }
    fn texts(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl Provider for Script {
    async fn complete(
        &self,
        system: &str,
        messages: &[Message],
        _: &[ToolDef],
    ) -> ark_model::Result<Completion> {
        let mut t = system.to_string();
        for m in messages {
            t.push('\n');
            t.push_str(&m.content);
        }
        self.seen.lock().unwrap().push(t);
        if let Some(g) = &self.gate {
            g.acquire().await.unwrap().forget();
        }
        let next = self.replies.lock().unwrap().pop_front();
        next.unwrap_or_else(|| Ok(answer("handled")))
    }
    fn describe(&self) -> String {
        "script".into()
    }
}

fn answer(text: &str) -> Completion {
    Completion {
        content: text.into(),
        tool_calls: vec![],
        usage: Usage::measured(100, 20),
    }
}

fn no_credit() -> ProviderError {
    ProviderError::OutOfCredit {
        provider: "script".into(),
        detail: "no credit left".into(),
    }
}

/// An app over one store; each `orch()` is what a new process builds over it.
struct App {
    _dir: tempfile::TempDir,
    ark: Ark,
    project: Project,
    chat: String,
}

impl App {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let ark = Ark::builder()
            .store_root(dir.path())
            .provider(Arc::new(ark_model::MockProvider::new("unused", vec![])))
            .planner(false)
            .critic(false)
            .build()
            .unwrap();
        let project = ark.create_project("p", "").unwrap();
        let chat = ark
            .start_session(&project.id)
            .unwrap()
            .id()
            .as_str()
            .to_string();
        App {
            _dir: dir,
            ark,
            project,
            chat,
        }
    }
    fn orch(&self, m: &Arc<Script>) -> Orchestrator {
        let mm = m.clone();
        Orchestrator::builder(&self.ark, &self.project)
            .configure_models(move |b| b.provider("m", mm as Arc<dyn Provider>))
            .build()
            .unwrap()
    }
    fn log(&self) -> Arc<EventLog> {
        EventLog::of(&self.ark.engine().store)
    }
    /// An orchestrator with an agent `w` and an automation for Lara's mail, reporting to the
    /// chat.
    async fn automated(&self, m: &Arc<Script>) -> (Orchestrator, EventAutomationRecord) {
        let o = self.orch(m);
        o.spawn(AgentSpec::new("w").model("m").tools(Vec::<String>::new()))
            .await
            .unwrap();
        let a = o
            .automate(
                "w",
                TaskSpec::new("reply in the same thread using budget.csv").report_to(&self.chat),
                lara(),
                AutomationAuthority::default(),
            )
            .unwrap();
        (o, a)
    }
    fn posts(&self) -> Vec<ark_runtime::Post> {
        ark_runtime::session::posts(self.ark.engine(), &self.chat, 0, 100)
            .unwrap()
            .into_iter()
            .map(|(_, p)| p)
            .collect()
    }
}

fn lara() -> EventPattern {
    EventPattern::new("fake", "office@example.com")
        .kind("message")
        .from("lara@example.com")
}

fn mail(key: &str, from: &str, text: &str) -> ExternalEvent {
    ExternalEvent {
        source: "fake".into(),
        account: "office@example.com".into(),
        key: key.into(),
        kind: "message".into(),
        direction: Direction::Inbound,
        actor: Some(from.into()),
        subject: Some(format!("thread-{key}")),
        at_ms: now(),
        label: format!("Mail from {from}"),
        fields: Default::default(),
        payload: Some(text.into()),
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

/// Wait until `o` has `n` runs of `automation` that ended, at most 10 s.
async fn ended(o: &Orchestrator, automation: &str, n: usize) -> Vec<TaskRecord> {
    for _ in 0..1000 {
        let runs = o.automation_runs(automation, 1000);
        if runs.iter().filter(|t| t.state.is_final()).count() >= n {
            return runs;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!(
        "{n} runs never ended: {:?}",
        o.automation_runs(automation, 1000)
    );
}

/// Let the evaluator and runs settle, then check nothing more started.
async fn settle() {
    tokio::time::sleep(Duration::from_millis(150)).await;
}

#[tokio::test]
async fn one_event_delivered_twice_makes_one_task_and_one_chat_result() {
    let app = App::new();
    let m = Script::new(vec![Ok(answer("Replied to Lara with the Q4 totals."))]);
    let (o, a) = app.automated(&m).await;
    let log = app.log();
    let e = mail("m1", "Lara@Example.com", "Can you send me the Q4 totals?");
    assert_eq!(log.ingest(vec![e.clone()]).unwrap().new, 1);
    assert_eq!(log.ingest(vec![e]).unwrap().repeated, 1);
    let runs = ended(&o, &a.id, 1).await;
    settle().await;
    assert_eq!(o.automation_runs(&a.id, 10).len(), 1, "one task");
    let run = &runs[0];
    assert_eq!(run.state, TaskState::Done);
    assert_eq!(run.automation.as_deref(), Some(a.id.as_str()));
    assert_eq!(run.revision, Some(1));
    assert!(run
        .context
        .contains("started by an event: Mail from Lara@Example.com"));
    assert!(run
        .context
        .contains("<<<event\nCan you send me the Q4 totals?\nevent>>>"));
    let posts = app.posts();
    assert_eq!(posts.len(), 1, "one chat result: {posts:?}");
    assert_eq!(posts[0].text, "Replied to Lara with the Q4 totals.");
    let rec = o.automation(&a.id).unwrap();
    assert_eq!((rec.matched, rec.runs), (1, 1));
    assert_eq!(rec.last_task.as_deref(), Some(run.id.as_str()));
}

#[tokio::test]
async fn events_from_the_acceptance_fixtures_start_one_run_each() {
    for (file, pattern) in [
        ("mail.json", include_str!("fixtures/automations/mail.json")),
        (
            "whatsapp.json",
            include_str!("fixtures/automations/whatsapp.json"),
        ),
    ] {
        let fixture: serde_json::Value = serde_json::from_str(pattern).unwrap();
        let app = App::new();
        let m = Script::new(vec![]);
        let o = app.orch(&m);
        o.spawn(AgentSpec::new("w").model("m").tools(Vec::<String>::new()))
            .await
            .unwrap();
        let p: EventPattern = serde_json::from_value(fixture["pattern"].clone()).unwrap();
        let a = o
            .automate(
                "w",
                TaskSpec::new("answer"),
                p,
                AutomationAuthority::default(),
            )
            .unwrap();
        let events: Vec<ExternalEvent> = fixture["expected"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| {
                let mut e: ExternalEvent = serde_json::from_value(v.clone()).unwrap();
                // The provider dated them before this test's automation existed.
                e.at_ms = now();
                e
            })
            .collect();
        app.log().ingest(events).unwrap();
        let want = fixture["runs"].as_u64().unwrap() as usize;
        ended(&o, &a.id, want).await;
        settle().await;
        assert_eq!(o.automation_runs(&a.id, 10).len(), want, "{file}");
    }
}

#[tokio::test]
async fn an_event_saved_before_the_automation_never_runs() {
    let app = App::new();
    let log = app.log();
    log.ingest(vec![mail("old", "lara@example.com", "before")])
        .unwrap();
    let m = Script::new(vec![]);
    let (o, a) = app.automated(&m).await;
    // A late re-delivery of an old message: saved now, but dated before the automation.
    let mut late = mail("late", "lara@example.com", "late");
    late.at_ms = now() - 3_600_000;
    log.ingest(vec![late]).unwrap();
    o.evaluate_events();
    settle().await;
    assert!(o.automation_runs(&a.id, 10).is_empty());
    log.ingest(vec![mail("new", "lara@example.com", "after")])
        .unwrap();
    let runs = ended(&o, &a.id, 1).await;
    assert!(runs[0].context.contains("after"));
}

#[tokio::test]
async fn pause_lets_events_go_and_resume_starts_from_now() {
    let app = App::new();
    let gate = Arc::new(Semaphore::new(0));
    let m = Script::gated(vec![], Some(gate.clone()));
    let (o, a) = app.automated(&m).await;
    let log = app.log();
    log.ingest(vec![
        mail("1", "lara@example.com", "one"),
        mail("2", "lara@example.com", "two"),
        mail("3", "lara@example.com", "three"),
    ])
    .unwrap();
    // The first runs (held at the model); two wait.
    for _ in 0..500 {
        if o.automation_pending(&a.id).len() == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(o.automation_pending(&a.id).len(), 2);
    let p = o.pause_automation(&a.id).unwrap();
    assert_eq!(p.state, AutomationState::Paused);
    assert!(o.automation_pending(&a.id).is_empty());
    let outcomes: Vec<Outcome> = o
        .automation_evaluations(&a.id)
        .into_iter()
        .map(|e| e.outcome)
        .collect();
    assert_eq!(
        outcomes
            .iter()
            .filter(|o| **o == Outcome::CancelledByPause)
            .count(),
        2,
        "{outcomes:?}"
    );
    // While paused, events are let go.
    log.ingest(vec![mail("4", "lara@example.com", "while paused")])
        .unwrap();
    // The run that was working goes on.
    gate.add_permits(1);
    let runs = ended(&o, &a.id, 1).await;
    assert!(runs[0].context.contains("one"));
    let r = o.resume_automation(&a.id).unwrap();
    assert_eq!((r.state, r.revision), (AutomationState::Listening, 2));
    settle().await;
    assert_eq!(
        o.automation_runs(&a.id, 10).len(),
        1,
        "nothing from the pause"
    );
    // A pause survives a restart.
    o.pause_automation(&a.id).unwrap();
    drop(o);
    let o = app.orch(&m);
    assert_eq!(o.automation(&a.id).unwrap().state, AutomationState::Paused);
    o.resume_automation(&a.id).unwrap();
    log.ingest(vec![mail("5", "lara@example.com", "after resume")])
        .unwrap();
    gate.add_permits(1);
    let runs = ended(&o, &a.id, 2).await;
    assert!(
        runs[0].context.contains("after resume"),
        "{:?}",
        runs[0].context
    );
}

#[tokio::test]
async fn a_change_cancels_waiting_events_and_only_later_events_match_the_new_words() {
    let app = App::new();
    let gate = Arc::new(Semaphore::new(0));
    let m = Script::gated(vec![], Some(gate.clone()));
    let (o, a) = app.automated(&m).await;
    let log = app.log();
    log.ingest(vec![
        mail("1", "lara@example.com", "one"),
        mail("2", "lara@example.com", "two"),
    ])
    .unwrap();
    for _ in 0..500 {
        if o.automation_pending(&a.id).len() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let c = o
        .change_automation(
            &a.id,
            AutomationChange {
                objective: Some("reply using prices.csv".into()),
                pattern: Some(
                    EventPattern::new("fake", "office@example.com")
                        .kind("message")
                        .from("ana@example.org"),
                ),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(c.revision, 2);
    assert!(o
        .automation_evaluations(&a.id)
        .iter()
        .any(|e| e.outcome == Outcome::CancelledByChange));
    gate.add_permits(1);
    let runs = ended(&o, &a.id, 1).await;
    assert_eq!(runs[0].revision, Some(1), "the working run kept its words");
    log.ingest(vec![
        mail("3", "lara@example.com", "lara again"),
        mail("4", "ana@example.org", "from ana"),
    ])
    .unwrap();
    gate.add_permits(1);
    let runs = ended(&o, &a.id, 2).await;
    assert_eq!(runs[0].revision, Some(2));
    assert!(runs[0].objective.contains("prices.csv"));
    assert!(runs[0].context.contains("from ana"));
    settle().await;
    assert_eq!(o.automation_runs(&a.id, 10).len(), 2);
}

#[tokio::test]
async fn stop_cancels_waiting_events_and_keeps_the_running_task_and_history() {
    let app = App::new();
    let gate = Arc::new(Semaphore::new(0));
    let m = Script::gated(vec![], Some(gate.clone()));
    let (o, a) = app.automated(&m).await;
    let log = app.log();
    log.ingest(vec![
        mail("1", "lara@example.com", "one"),
        mail("2", "lara@example.com", "two"),
    ])
    .unwrap();
    for _ in 0..500 {
        if o.automation_pending(&a.id).len() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let evaluations_before = o.automation_evaluations(&a.id);
    let gone = o.unautomate(&a.id).unwrap();
    assert_eq!(gone.id, a.id);
    assert!(o.automation(&a.id).is_none());
    assert!(app
        .ark
        .engine()
        .store
        .get_record("event_automation", &a.id)
        .unwrap()
        .is_none());
    let stopped: Vec<_> = o
        .automation_evaluations(&a.id)
        .into_iter()
        .filter(|e| e.outcome == Outcome::CancelledByStop)
        .collect();
    assert_eq!(stopped.len(), 1, "{evaluations_before:?}");
    gate.add_permits(1);
    let runs = ended(&o, &a.id, 1).await;
    assert_eq!(runs[0].state, TaskState::Done, "the running task finished");
    log.ingest(vec![mail("3", "lara@example.com", "after stop")])
        .unwrap();
    settle().await;
    assert_eq!(o.automation_runs(&a.id, 10).len(), 1);
    assert!(
        o.automate(
            "w",
            TaskSpec::new("again"),
            lara(),
            AutomationAuthority::default()
        )
        .is_ok(),
        "the agent is free again"
    );
}

#[tokio::test]
async fn full_then_drain_keeps_the_revision_and_the_queue_in_order() {
    let app = App::new();
    let log = app.log();
    log.set_limits(LogLimits {
        queue: 2,
        ..LogLimits::default()
    });
    let gate = Arc::new(Semaphore::new(0));
    let m = Script::gated(vec![], Some(gate.clone()));
    let (o, a) = app.automated(&m).await;
    log.ingest(
        (1..=5)
            .map(|i| mail(&i.to_string(), "lara@example.com", &format!("number {i}")))
            .collect(),
    )
    .unwrap();
    for _ in 0..500 {
        if o.automation(&a.id).unwrap().state == AutomationState::Full {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let full = o.automation(&a.id).unwrap();
    assert_eq!(full.state, AutomationState::Full);
    assert_eq!(o.automation_pending(&a.id).len(), 2, "one runs, two wait");
    gate.add_permits(5);
    let runs = ended(&o, &a.id, 5).await;
    let order: Vec<String> = runs
        .iter()
        .rev()
        .map(|t| {
            t.context
                .lines()
                .find(|l| l.starts_with("number"))
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(
        order,
        (1..=5).map(|i| format!("number {i}")).collect::<Vec<_>>(),
        "arrival order"
    );
    let after = o.automation(&a.id).unwrap();
    assert_eq!(after.revision, 1, "Full is not a pause: same revision");
    assert_eq!(after.state, AutomationState::Listening);
    assert!(runs.iter().all(|t| t.revision == Some(1)));
}

#[tokio::test]
async fn full_then_discard_cancels_the_queue_and_cuts_over() {
    let app = App::new();
    let log = app.log();
    log.set_limits(LogLimits {
        queue: 1,
        ..LogLimits::default()
    });
    let gate = Arc::new(Semaphore::new(0));
    let m = Script::gated(vec![], Some(gate.clone()));
    let (o, a) = app.automated(&m).await;
    log.ingest(
        (1..=4)
            .map(|i| mail(&i.to_string(), "lara@example.com", &format!("number {i}")))
            .collect(),
    )
    .unwrap();
    for _ in 0..500 {
        if o.automation(&a.id).unwrap().state == AutomationState::Full {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let d = o.discard_pending(&a.id).unwrap();
    assert_eq!((d.state, d.revision), (AutomationState::Listening, 2));
    assert!(o.automation_pending(&a.id).is_empty());
    assert!(o
        .automation_evaluations(&a.id)
        .iter()
        .any(|e| e.outcome == Outcome::Discarded));
    gate.add_permits(1);
    ended(&o, &a.id, 1).await;
    settle().await;
    assert_eq!(
        o.automation_runs(&a.id, 10).len(),
        1,
        "events 3 and 4, never read, went with the cutover"
    );
    log.ingest(vec![mail("5", "lara@example.com", "number 5")])
        .unwrap();
    gate.add_permits(1);
    let runs = ended(&o, &a.id, 2).await;
    assert!(runs[0].context.contains("number 5"));
}

#[tokio::test]
async fn the_account_inbox_limit_refuses_until_events_are_read() {
    let app = App::new();
    let log = app.log();
    log.set_limits(LogLimits {
        queue: 1,
        inbox: 2,
        ..LogLimits::default()
    });
    let gate = Arc::new(Semaphore::new(0));
    let m = Script::gated(vec![], Some(gate.clone()));
    let (o, a) = app.automated(&m).await;
    log.ingest(vec![
        mail("1", "lara@example.com", "x"),
        mail("2", "lara@example.com", "x"),
    ])
    .unwrap();
    // 1 runs, 2 waits: the queue is at its limit. 3 makes it Full.
    for _ in 0..500 {
        if o.automation_pending(&a.id).len() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    log.ingest(vec![mail("3", "lara@example.com", "x")])
        .unwrap();
    for _ in 0..500 {
        if o.automation(&a.id).unwrap().state == AutomationState::Full {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    log.ingest(vec![mail("4", "lara@example.com", "x")])
        .unwrap();
    let e = log
        .ingest(vec![mail("5", "lara@example.com", "x")])
        .unwrap_err();
    assert!(matches!(e, IngestError::Full(_)), "{e}");
    assert!(e.retryable());
    assert_eq!(log.waiting("fake", "office@example.com"), 2);
    // A re-delivery of a saved event is still harmless while full.
    assert_eq!(
        log.ingest(vec![mail("4", "lara@example.com", "x")])
            .unwrap()
            .repeated,
        1
    );
    gate.add_permits(4);
    ended(&o, &a.id, 4).await;
    assert_eq!(
        log.ingest(vec![mail("5", "lara@example.com", "x")])
            .unwrap()
            .new,
        1
    );
}

/// The records a crash leaves between durable steps, then a new process over them.
#[tokio::test]
async fn a_crash_between_any_two_steps_makes_no_second_run() {
    let app = App::new();
    let m = Script::new(vec![]);
    let (o, a) = app.automated(&m).await;
    let store = app.ark.engine().store.clone();
    // 1. Saved, never evaluated (the process died after ingest): runs once after the restart.
    o.pause_automation(&a.id).unwrap();
    o.resume_automation(&a.id).unwrap();
    drop(o);
    let log = app.log();
    log.ingest(vec![mail("1", "lara@example.com", "one")])
        .unwrap();
    // The feed did not save its cursor before the crash: it delivers the event again.
    log.ingest(vec![mail("1", "lara@example.com", "one")])
        .unwrap();
    drop(log);
    let o = app.orch(&m);
    ended(&o, &a.id, 1).await;
    settle().await;
    assert_eq!(o.automation_runs(&a.id, 10).len(), 1);
    drop(o);

    // 2. Matched and saved as pending, no task yet: the restart starts it once.
    let o = app.orch(&m);
    let rec = o.automation(&a.id).unwrap();
    o.pause_automation(&a.id).unwrap();
    drop(o);
    let log = app.log();
    log.ingest(vec![mail("2", "lara@example.com", "two")])
        .unwrap();
    let e2 = log.events_after("fake", "office@example.com", rec.evaluated_seq, 10)[0].clone();
    let mut live = rec.clone();
    live.state = AutomationState::Listening;
    live.evaluated_seq = e2.seq;
    let eval = json!({
        "id": "v-crash2", "automation": a.id, "revision": live.revision, "event": e2.id,
        "seq": e2.seq, "at_ms": now(), "outcome": "pending"
    });
    store
        .put_records(&[
            (
                "event_automation",
                &a.id,
                app.project.id.as_str(),
                &serde_json::to_value(&live).unwrap(),
            ),
            (
                "event_evaluation",
                "v-crash2",
                app.project.id.as_str(),
                &eval,
            ),
        ])
        .unwrap();
    drop(log);
    let o = app.orch(&m);
    ended(&o, &a.id, 2).await;
    settle().await;
    assert_eq!(o.automation_runs(&a.id, 10).len(), 2);
    drop(o);

    // 3. The task was saved, the evaluation not marked started: adopted, not made again.
    let o = app.orch(&m);
    let rec = o.automation(&a.id).unwrap();
    o.pause_automation(&a.id).unwrap();
    drop(o);
    let log = app.log();
    log.ingest(vec![mail("3", "lara@example.com", "three")])
        .unwrap();
    let e3 = log.events_after("fake", "office@example.com", rec.evaluated_seq, 10)[0].clone();
    let mut live = rec.clone();
    live.state = AutomationState::Listening;
    live.evaluated_seq = e3.seq;
    let task = runs_task(&a.id, &e3.id);
    let eval = json!({
        "id": "v-crash3", "automation": a.id, "revision": live.revision, "event": e3.id,
        "seq": e3.seq, "at_ms": now(), "outcome": "pending"
    });
    let made = json!({
        "id": task, "project": app.project.id.to_string(), "owner": "w",
        "objective": "reply", "origin": { "by": "external" }, "state": "done",
        "result": "done before the crash", "created_ms": now(), "finished_ms": now(),
        "automation": a.id, "revision": live.revision, "event": e3.id
    });
    store
        .put_records(&[
            (
                "event_automation",
                &a.id,
                app.project.id.as_str(),
                &serde_json::to_value(&live).unwrap(),
            ),
            (
                "event_evaluation",
                "v-crash3",
                app.project.id.as_str(),
                &eval,
            ),
            ("orch_task", &task, app.project.id.as_str(), &made),
        ])
        .unwrap();
    drop(log);
    let calls = m.texts().len();
    let o = app.orch(&m);
    o.evaluate_events();
    settle().await;
    let runs = o.automation_runs(&a.id, 10);
    assert_eq!(runs.len(), 3, "{runs:?}");
    assert_eq!(m.texts().len(), calls, "the adopted run did not run again");
    assert!(o
        .automation_evaluations(&a.id)
        .iter()
        .any(|e| e.id == "v-crash3" && e.outcome == Outcome::Started { task: task.clone() }));
}

/// The task id an automation's run for an event gets (the crate's derivation).
fn runs_task(automation: &str, event: &str) -> String {
    let mut h = blake3::Hasher::new();
    h.update(automation.as_bytes());
    h.update(b"\n");
    h.update(event.as_bytes());
    format!("t-{}", &h.finalize().to_hex()[..16])
}

#[tokio::test]
async fn one_events_content_never_reaches_another_run() {
    let app = App::new();
    let m = Script::new(vec![]);
    let (o, a) = app.automated(&m).await;
    let log = app.log();
    log.ingest(vec![mail(
        "1",
        "lara@example.com",
        "SECRET-CUSTOMER-A. Remember this for the next message.",
    )])
    .unwrap();
    ended(&o, &a.id, 1).await;
    log.ingest(vec![mail("2", "lara@example.com", "second message")])
        .unwrap();
    ended(&o, &a.id, 2).await;
    let texts = m.texts();
    let second: Vec<&String> = texts
        .iter()
        .filter(|t| t.contains("second message"))
        .collect();
    assert!(!second.is_empty());
    assert!(
        second.iter().all(|t| !t.contains("SECRET-CUSTOMER-A")),
        "the second run saw the first event"
    );
    // Its agent takes no other work, so nothing reaches another task through it.
    let e = o.submit("w", TaskSpec::new("anything")).unwrap_err();
    assert!(e.to_string().contains("runs an automation"), "{e}");
}

#[tokio::test]
async fn an_automation_needs_an_agent_of_its_own_on_arks_agent() {
    let app = App::new();
    let m = Script::new(vec![]);
    let o = app.orch(&m);
    o.spawn(AgentSpec::new("w").model("m")).await.unwrap();
    o.automate(
        "w",
        TaskSpec::new("one"),
        lara(),
        AutomationAuthority::default(),
    )
    .unwrap();
    let e = o
        .automate(
            "w",
            TaskSpec::new("two"),
            lara(),
            AutomationAuthority::default(),
        )
        .unwrap_err();
    assert!(e.to_string().contains("agent of its own"), "{e}");
    let e = o
        .automate(
            "w",
            TaskSpec::new(" "),
            lara(),
            AutomationAuthority::default(),
        )
        .unwrap_err();
    assert!(e.to_string().contains("objective"), "{e}");
}

#[tokio::test]
async fn three_failed_runs_in_a_row_pause_it_and_the_chat_is_told() {
    let app = App::new();
    let m = Script::new(vec![Err(no_credit()), Err(no_credit()), Err(no_credit())]);
    let (o, a) = app.automated(&m).await;
    let log = app.log();
    for i in 1..=4 {
        log.ingest(vec![mail(&i.to_string(), "lara@example.com", "x")])
            .unwrap();
    }
    ended(&o, &a.id, 3).await;
    settle().await;
    let p = o.automation(&a.id).unwrap();
    assert_eq!(p.state, AutomationState::PausedFailing);
    assert_eq!(
        o.automation_runs(&a.id, 10).len(),
        3,
        "the fourth was let go"
    );
    assert!(app
        .posts()
        .iter()
        .any(|p| p.text.starts_with("Paused after 3 runs")));
    let r = o.resume_automation(&a.id).unwrap();
    assert_eq!((r.state, r.failures), (AutomationState::Listening, 0));
}

#[tokio::test]
async fn projects_with_automations_open_when_an_app_starts() {
    let app = App::new();
    let m = Script::new(vec![]);
    let (_o, _a) = app.automated(&m).await;
    let store = &app.ark.engine().store;
    assert_eq!(automated_projects(store), vec![app.project.id.to_string()]);
    assert!(projects_with_work(store).contains(&app.project.id.to_string()));
    assert!(
        scheduled_projects(store).is_empty(),
        "schedules are their own"
    );
}

#[tokio::test]
async fn an_agent_that_hands_work_on_cannot_run_an_automation() {
    let app = App::new();
    let m = Script::new(vec![]);
    let o = app.orch(&m);
    o.spawn(AgentSpec::new("helper").model("m")).await.unwrap();
    o.spawn(AgentSpec::new("lead").model("m").delegates_to(["helper"]))
        .await
        .unwrap();
    let e = o
        .automate(
            "lead",
            TaskSpec::new("x"),
            lara(),
            AutomationAuthority::default(),
        )
        .unwrap_err();
    assert!(e.to_string().contains("keeps each event to itself"), "{e}");
}
