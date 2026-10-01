//! Model-call retries: bounded, observable, never after text was shown, never re-running a
//! tool, and cancellable while waiting.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ark_core::{ActionDescriptor, RiskTier};
use ark_model::{
    Completion, Message, Provider, ProviderError, RetryPolicy, TextSink, ToolCall, ToolDef, Usage,
};
use ark_runtime::{AgentProfile, CancelToken, Engine, Harness, HarnessEvent, ModelRouter};
use ark_tools::{ActionOutput, Adapter, Registry, RunCtx};
use serde_json::json;

enum Step {
    Ok(Completion),
    Fail(fn() -> ProviderError),
    /// Stream some text, then fail.
    TextThenFail(&'static str, fn() -> ProviderError),
}

struct Flaky {
    steps: Mutex<VecDeque<Step>>,
    calls: AtomicUsize,
}

impl Flaky {
    fn new(steps: Vec<Step>) -> Arc<Self> {
        Arc::new(Flaky {
            steps: Mutex::new(steps.into()),
            calls: AtomicUsize::new(0),
        })
    }
}

#[async_trait::async_trait]
impl Provider for Flaky {
    async fn complete(
        &self,
        s: &str,
        m: &[Message],
        t: &[ToolDef],
    ) -> ark_model::Result<Completion> {
        self.complete_streaming(s, m, t, &|_: &str| {}).await
    }
    async fn complete_streaming(
        &self,
        _s: &str,
        _m: &[Message],
        _t: &[ToolDef],
        on_text: TextSink<'_>,
    ) -> ark_model::Result<Completion> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let step = self.steps.lock().unwrap().pop_front();
        match step {
            Some(Step::Ok(c)) => {
                if !c.content.is_empty() {
                    on_text(&c.content);
                }
                Ok(c)
            }
            Some(Step::Fail(e)) => Err(e()),
            Some(Step::TextThenFail(t, e)) => {
                on_text(t);
                Err(e())
            }
            None => Ok(answer("fallback")),
        }
    }
    fn describe(&self) -> String {
        "flaky".into()
    }
}

fn overloaded() -> ProviderError {
    ProviderError::Status {
        provider: "x".into(),
        status: 529,
        detail: "overloaded".into(),
        retry_after: None,
    }
}

fn bad_request() -> ProviderError {
    ProviderError::Status {
        provider: "x".into(),
        status: 400,
        detail: "bad".into(),
        retry_after: None,
    }
}

fn answer(t: &str) -> Completion {
    Completion {
        content: t.into(),
        tool_calls: vec![],
        usage: Usage::measured(10, 5),
    }
}

fn call(name: &str) -> Completion {
    Completion {
        content: String::new(),
        tool_calls: vec![ToolCall {
            id: "c1".into(),
            name: name.into(),
            arguments: json!({ "text": "x" }),
        }],
        usage: Usage::measured(10, 5),
    }
}

struct Saver(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl Adapter for Saver {
    fn name(&self) -> &str {
        "notes"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![ActionDescriptor::new(
            "notes.save",
            "save",
            RiskTier::Mutate,
        )]
    }
    async fn run(
        &self,
        _a: &str,
        _i: serde_json::Value,
        _c: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(ActionOutput::json(json!({ "saved": true })))
    }
}

fn fast_policy(n: u32) -> RetryPolicy {
    RetryPolicy {
        max_retries: n,
        base_delay: Duration::from_millis(5),
        max_delay: Duration::from_millis(20),
    }
}

struct Seen {
    retries: Vec<u32>,
    text: String,
    model_retries: Vec<u32>,
}

async fn run(
    model: Arc<Flaky>,
    policy: RetryPolicy,
    cancel: Option<CancelToken>,
) -> (
    Result<ark_runtime::HarnessOutcome, ark_runtime::AgentError>,
    Seen,
    Arc<AtomicUsize>,
) {
    let saves = Arc::new(AtomicUsize::new(0));
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open_with(
        dir.path(),
        Registry::new().with(Arc::new(Saver(saves.clone()))),
    )
    .unwrap();
    let project = engine.create_project("t", "").unwrap().id;
    let p: Arc<dyn Provider> = model;
    let mut seen = Seen {
        retries: vec![],
        text: String::new(),
        model_retries: vec![],
    };
    let out = Harness::new(ModelRouter::single(p))
        .planner(false)
        .critic(false)
        .allow_mutate(true)
        .retry_policy(policy)
        .cancel(cancel)
        .ask(
            &engine,
            &AgentProfile::builder().core_actions(["notes.save"]).build(),
            &project,
            "save it",
            |e| match e {
                HarnessEvent::Retry { attempt, .. } => seen.retries.push(attempt),
                HarnessEvent::TextDelta { text } => seen.text.push_str(&text),
                HarnessEvent::ModelCall { retries, .. } => seen.model_retries.push(retries),
                _ => {}
            },
        )
        .await;
    (out, seen, saves)
}

#[tokio::test]
async fn transient_failures_are_retried_and_reported() {
    let m = Flaky::new(vec![
        Step::Fail(overloaded),
        Step::Fail(overloaded),
        Step::Ok(answer("done")),
    ]);
    let (out, seen, _) = run(m.clone(), fast_policy(2), None).await;
    let out = out.unwrap();
    assert_eq!(out.answer, "done");
    assert_eq!(seen.retries, vec![1, 2]);
    assert_eq!(seen.model_retries, vec![2]);
    assert_eq!(out.metrics.provider_retries, 2);
    assert_eq!(m.calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn retries_stop_at_the_limit() {
    let m = Flaky::new(vec![
        Step::Fail(overloaded),
        Step::Fail(overloaded),
        Step::Fail(overloaded),
    ]);
    let (out, seen, _) = run(m.clone(), fast_policy(2), None).await;
    assert!(out.is_err());
    assert_eq!(seen.retries, vec![1, 2]);
    assert_eq!(m.calls.load(Ordering::SeqCst), 3, "one try and two retries");
}

#[tokio::test]
async fn a_bad_request_is_not_retried() {
    let m = Flaky::new(vec![Step::Fail(bad_request), Step::Ok(answer("never"))]);
    let (out, seen, _) = run(m.clone(), fast_policy(2), None).await;
    assert!(out.is_err());
    assert!(seen.retries.is_empty());
    assert_eq!(m.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn no_retry_after_text_reached_the_user() {
    let m = Flaky::new(vec![
        Step::TextThenFail("half an ans", overloaded),
        Step::Ok(answer("whole answer")),
    ]);
    let (out, seen, _) = run(m.clone(), fast_policy(2), None).await;
    assert!(
        out.is_err(),
        "the turn fails instead of streaming a second answer"
    );
    assert!(seen.retries.is_empty());
    assert_eq!(seen.text, "half an ans", "no duplicated output");
}

#[tokio::test]
async fn a_retried_model_call_never_runs_a_tool_twice() {
    // The model asks to save; the save runs; the next model call fails once and is retried.
    let m = Flaky::new(vec![
        Step::Ok(call("notes.save")),
        Step::Fail(overloaded),
        Step::Ok(answer("saved")),
    ]);
    let (out, seen, saves) = run(m.clone(), fast_policy(2), None).await;
    assert_eq!(out.unwrap().answer, "saved");
    assert_eq!(seen.retries, vec![1]);
    assert_eq!(
        saves.load(Ordering::SeqCst),
        1,
        "the state-changing tool ran exactly once"
    );
}

#[tokio::test]
async fn cancelling_during_the_backoff_returns_at_once() {
    let token = CancelToken::new();
    let t = token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        t.cancel();
    });
    let slow = RetryPolicy {
        max_retries: 3,
        base_delay: Duration::from_secs(10),
        max_delay: Duration::from_secs(10),
    };
    let m = Flaky::new(vec![Step::Fail(overloaded), Step::Ok(answer("late"))]);
    let started = Instant::now();
    let (out, _, _) = run(m, slow, Some(token)).await;
    assert!(out.unwrap().cancelled);
    assert!(started.elapsed() < Duration::from_secs(2));
}
