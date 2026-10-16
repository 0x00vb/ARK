//! Model-call retries: bounded, observable, never re-running a tool, cancellable while waiting,
//! and possible after text was shown, which is then withdrawn (ADR 0049). A reply the model
//! wrote that cannot be used is not resent: the model is told why and sends it again, a
//! bounded number of times (ADR 0050).

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
    /// The messages of each request, in order.
    requests: Mutex<Vec<Vec<Message>>>,
}

impl Flaky {
    fn new(steps: Vec<Step>) -> Arc<Self> {
        Arc::new(Flaky {
            steps: Mutex::new(steps.into()),
            calls: AtomicUsize::new(0),
            requests: Mutex::new(Vec::new()),
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
        m: &[Message],
        _t: &[ToolDef],
        on_text: TextSink<'_>,
    ) -> ark_model::Result<Completion> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests.lock().unwrap().push(m.to_vec());
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
        offline: Duration::ZERO,
    }
}

struct Seen {
    retries: Vec<u32>,
    /// Text as a front-end that takes withdrawn text back shows it.
    text: String,
    withdrawn: Vec<String>,
    model_retries: Vec<u32>,
    /// Streamed text, withdrawals, retries and the answer, in order.
    order: Vec<String>,
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
        withdrawn: vec![],
        model_retries: vec![],
        order: vec![],
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
                HarnessEvent::Retry { attempt, .. } => {
                    seen.order.push(format!("retry {attempt}"));
                    seen.retries.push(attempt)
                }
                HarnessEvent::TextDelta { text } => {
                    seen.order.push(format!("text {text}"));
                    seen.text.push_str(&text)
                }
                HarnessEvent::TextWithdrawn { text } => {
                    seen.order.push(format!("withdrawn {text}"));
                    let kept = seen.text.strip_suffix(text.as_str()).map(str::to_string);
                    seen.text = kept.expect("only shown text is withdrawn");
                    seen.withdrawn.push(text)
                }
                HarnessEvent::Answer { text, streamed } => seen
                    .order
                    .push(format!("answer {text} streamed={streamed}")),
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

fn malformed() -> ProviderError {
    ProviderError::Malformed {
        provider: "x".into(),
        detail: "arguments for notes.save: not JSON: expected `,` or `}` at line 1 column 1573"
            .into(),
    }
}

#[tokio::test]
async fn text_then_a_failure_is_withdrawn_and_the_call_retried() {
    let m = Flaky::new(vec![
        Step::TextThenFail("half an ans", overloaded),
        Step::Ok(answer("whole answer")),
    ]);
    let (out, seen, _) = run(m.clone(), fast_policy(2), None).await;
    assert_eq!(out.unwrap().answer, "whole answer");
    assert_eq!(
        seen.order,
        [
            "text half an ans",
            "withdrawn half an ans",
            "retry 1",
            "text whole answer",
            "answer whole answer streamed=true"
        ]
    );
    assert_eq!(seen.text, "whole answer", "nothing shown twice");
    assert_eq!(m.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_malformed_reply_after_its_note_recovers_and_the_tool_runs_once() {
    // Codex's case: the reply's note streamed, then its tool arguments did not parse.
    let m = Flaky::new(vec![
        Step::TextThenFail("Plan: (1) keep the textarea", malformed),
        Step::Ok(call("notes.save")),
        Step::Ok(answer("saved")),
    ]);
    let (out, seen, saves) = run(m.clone(), fast_policy(2), None).await;
    assert_eq!(out.unwrap().answer, "saved");
    assert_eq!(seen.withdrawn, ["Plan: (1) keep the textarea"]);
    assert_eq!(seen.retries, vec![1]);
    assert_eq!(saves.load(Ordering::SeqCst), 1, "the save ran once");
    assert_eq!(seen.text, "saved");
}

#[tokio::test]
async fn a_failure_after_text_that_follows_a_completed_change_does_not_repeat_it() {
    let m = Flaky::new(vec![
        Step::Ok(call("notes.save")),
        Step::TextThenFail("Saved. The no", overloaded),
        Step::Ok(answer("Saved. The note is stored.")),
    ]);
    let (out, seen, saves) = run(m.clone(), fast_policy(2), None).await;
    assert_eq!(out.unwrap().answer, "Saved. The note is stored.");
    assert_eq!(
        saves.load(Ordering::SeqCst),
        1,
        "the completed save is not run again"
    );
    assert_eq!(seen.text, "Saved. The note is stored.");
}

#[tokio::test]
async fn every_failed_attempt_withdraws_its_own_text() {
    // Several bytes per character; a shorter retry; then a longer one.
    let m = Flaky::new(vec![
        Step::TextThenFail("Résumé ✓ — first try, long", overloaded),
        Step::TextThenFail("Ré", malformed),
        Step::Ok(answer("Résumé ✓ done")),
    ]);
    let (out, seen, _) = run(m.clone(), fast_policy(3), None).await;
    assert_eq!(out.unwrap().answer, "Résumé ✓ done");
    assert_eq!(seen.withdrawn, ["Résumé ✓ — first try, long", "Ré"]);
    // A transport retry, then the first repair of an unusable reply.
    assert_eq!(seen.retries, vec![1, 1]);
    assert_eq!(seen.text, "Résumé ✓ done");
}

#[tokio::test]
async fn a_bad_request_after_text_is_not_retried_and_nothing_is_withdrawn() {
    let m = Flaky::new(vec![
        Step::TextThenFail("half an ans", bad_request),
        Step::Ok(answer("never")),
    ]);
    let (out, seen, _) = run(m.clone(), fast_policy(2), None).await;
    assert!(out.is_err());
    assert!(seen.retries.is_empty() && seen.withdrawn.is_empty());
    assert_eq!(m.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn retries_after_text_stop_at_the_limit() {
    let m = Flaky::new(vec![
        Step::TextThenFail("a", overloaded),
        Step::TextThenFail("b", overloaded),
        Step::TextThenFail("c", overloaded),
        Step::Ok(answer("never")),
    ]);
    let (out, seen, _) = run(m.clone(), fast_policy(2), None).await;
    assert!(out.is_err());
    assert_eq!(
        seen.withdrawn,
        ["a", "b"],
        "the last attempt's text stays with the error"
    );
    assert_eq!(m.calls.load(Ordering::SeqCst), 3);
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
        offline: Duration::ZERO,
    };
    let m = Flaky::new(vec![Step::Fail(overloaded), Step::Ok(answer("late"))]);
    let started = Instant::now();
    let (out, _, _) = run(m, slow, Some(token)).await;
    assert!(out.unwrap().cancelled);
    assert!(started.elapsed() < Duration::from_secs(2));
}

fn refused() -> ProviderError {
    ProviderError::Refused {
        provider: "x".into(),
        detail: "the reply schema was refused (invalid_json_schema)".into(),
    }
}

/// The text of every message of request `i`.
fn request_text(m: &Flaky, i: usize) -> String {
    m.requests.lock().unwrap()[i]
        .iter()
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn an_unusable_reply_becomes_feedback_and_the_model_sends_it_again() {
    // The owner's case: the note streamed, then the edit's arguments did not parse.
    let m = Flaky::new(vec![
        Step::TextThenFail("Plan: (1) keep the textarea", malformed),
        Step::Ok(call("notes.save")),
        Step::Ok(answer("saved")),
    ]);
    let (out, seen, saves) = run(m.clone(), fast_policy(0), None).await;
    assert_eq!(
        out.unwrap().answer,
        "saved",
        "no transport retry was needed"
    );
    assert_eq!(seen.withdrawn, ["Plan: (1) keep the textarea"]);
    let again = request_text(&m, 1);
    assert!(
        again.contains("Your last reply could not be used"),
        "{again}"
    );
    assert!(
        again.contains("column 1573"),
        "the model is told what was wrong: {again}"
    );
    assert!(
        again.contains("Plan: (1) keep the textarea"),
        "its own plan stays with it: {again}"
    );
    assert_eq!(saves.load(Ordering::SeqCst), 1);
    assert_eq!(m.calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn three_unusable_replies_in_a_row_end_the_turn() {
    let m = Flaky::new(vec![
        Step::Fail(malformed),
        Step::Fail(malformed),
        Step::Fail(malformed),
        Step::Ok(answer("never")),
    ]);
    let (out, seen, _) = run(m.clone(), fast_policy(2), None).await;
    match out {
        Err(ark_runtime::AgentError::UnusableReply { detail, .. }) => {
            assert!(detail.contains("column 1573"))
        }
        Err(e) => panic!("expected an unusable reply, got {e:?}"),
        Ok(o) => panic!("expected an unusable reply, got the answer {:?}", o.answer),
    }
    assert_eq!(seen.retries, vec![1, 2], "two repairs, then the turn ends");
    assert_eq!(m.calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn repairs_are_counted_in_a_row_and_bounded_per_turn() {
    // Two in a row, a good step, two more: four in the turn. A fifth ends it.
    let m = Flaky::new(vec![
        Step::Fail(malformed),
        Step::Fail(malformed),
        Step::Ok(call("notes.save")),
        Step::Fail(malformed),
        Step::Fail(malformed),
        Step::Ok(call("notes.save")),
        Step::Fail(malformed),
        Step::Ok(answer("never")),
    ]);
    let (out, seen, _) = run(m.clone(), fast_policy(2), None).await;
    assert!(
        matches!(out, Err(ark_runtime::AgentError::UnusableReply { .. })),
        "{:?}",
        out.as_ref().err()
    );
    assert_eq!(seen.retries, vec![1, 2, 3, 4]);
    assert_eq!(m.calls.load(Ordering::SeqCst), 7);
}

#[tokio::test]
async fn a_refused_schema_is_retried_without_telling_the_model() {
    let m = Flaky::new(vec![Step::Fail(refused), Step::Ok(answer("done"))]);
    let (out, seen, _) = run(m.clone(), fast_policy(2), None).await;
    assert_eq!(out.unwrap().answer, "done");
    assert_eq!(seen.retries, vec![1]);
    assert!(!request_text(&m, 1).contains("could not be used"));
    assert_eq!(
        request_text(&m, 0),
        request_text(&m, 1),
        "sent again as it was"
    );
}

#[tokio::test]
async fn local_text_inference_refuses_agent_tools_without_retry_or_spawn() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open_with(
        dir.path(),
        Registry::new().with(Arc::new(Saver(Arc::new(AtomicUsize::new(0))))),
    )
    .unwrap();
    let project = engine
        .create_project("local text capability", "")
        .unwrap()
        .id;
    // Neither path exists. Tool refusal must happen before executable lookup or model access.
    let config = ark_model::InferenceConfig::new(
        dir.path().join("missing-worker"),
        dir.path().join("missing.gguf"),
    );
    let provider: Arc<dyn Provider> = Arc::new(ark_model::Inference::new(config).unwrap());
    let mut retries = 0;
    let result = Harness::new(ModelRouter::single(provider))
        .planner(false)
        .critic(false)
        .allow_mutate(true)
        .retry_policy(fast_policy(3))
        .ask(
            &engine,
            &AgentProfile::builder().core_actions(["notes.save"]).build(),
            &project,
            "save it",
            |event| {
                if matches!(event, HarnessEvent::Retry { .. }) {
                    retries += 1;
                }
            },
        )
        .await;
    let error = match result {
        Err(error) => error.to_string(),
        Ok(_) => panic!("tool request unexpectedly succeeded"),
    };
    assert!(error.contains("without tools or images"), "{error}");
    assert_eq!(retries, 0);
}
