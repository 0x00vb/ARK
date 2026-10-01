//! Adversarial models against the stall guard. A model that repeats itself must be stopped
//! within a few steps; a model doing varied, real work must not be.

use std::sync::Arc;

use ark_core::{ActionDescriptor, RiskTier};
use ark_model::{Completion, MockProvider, Provider, ToolCall, Usage};
use ark_runtime::{AgentProfile, Engine, Harness, HarnessEvent, ModelRouter};
use ark_tools::{ActionOutput, Adapter, AdapterError, Registry, RunCtx};
use serde_json::json;

struct Svc;

#[async_trait::async_trait]
impl Adapter for Svc {
    fn name(&self) -> &str {
        "svc"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![
            ActionDescriptor::new("svc.read", "read a key", RiskTier::Read),
            ActionDescriptor::new("svc.down", "always fails", RiskTier::Read),
        ]
    }
    async fn run(
        &self,
        action: &str,
        input: serde_json::Value,
        _c: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        if action == "svc.down" {
            return Err(AdapterError::bad_input(action, "connection refused"));
        }
        Ok(ActionOutput::json(
            json!({ "key": input["key"], "value": 1 }),
        ))
    }
}

/// A model that answers every request with `next(request number)`; it never answers on its own.
fn looping(
    next: impl Fn(usize) -> (String, serde_json::Value) + Send + Sync + 'static,
) -> Arc<MockProvider> {
    let n = std::sync::atomic::AtomicUsize::new(0);
    Arc::new(MockProvider::new("loop", vec![]).with_fallback(move |req| {
        if req.tools.is_empty() {
            return Completion {
                content: "best answer so far; unknown.".into(),
                tool_calls: vec![],
                usage: Usage::measured(10, 5),
            };
        }
        let i = n.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let (name, arguments) = next(i);
        Completion {
            content: String::new(),
            tool_calls: vec![ToolCall {
                id: format!("c{i}"),
                name,
                arguments,
            }],
            usage: Usage::measured(10, 5),
        }
    }))
}

struct Run {
    steps: usize,
    answered: bool,
    nudges: usize,
    stops: usize,
}

async fn run(model: Arc<MockProvider>) -> Run {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open_with(dir.path(), Registry::new().with(Arc::new(Svc))).unwrap();
    let project = engine.create_project("t", "").unwrap();
    let provider: Arc<dyn Provider> = model;
    let profile = AgentProfile::builder()
        .core_actions(["svc.read", "svc.down"])
        .build();
    let (mut nudges, mut stops) = (0, 0);
    let out = Harness::new(ModelRouter::single(provider))
        .planner(false)
        .critic(false)
        .ask(
            &engine,
            &profile,
            &project.id,
            "investigate why the service is slow and explain",
            |e| {
                if let HarnessEvent::Stall { stopped, .. } = e {
                    if stopped {
                        stops += 1;
                    } else {
                        nudges += 1;
                    }
                }
            },
        )
        .await
        .unwrap();
    Run {
        steps: out.steps,
        answered: !out.answer.is_empty(),
        nudges,
        stops,
    }
}

/// The rule: five steps in a row with nothing new end the loop, so a run stops at most five
/// steps after its last new piece of information.
fn assert_stopped(r: &Run, what: &str, max_steps: usize) {
    assert!(r.steps <= max_steps, "{what}: ran {} steps", r.steps);
    assert!(r.answered, "{what}: a final answer is still forced");
    assert_eq!(r.nudges, 1, "{what}: nudged once before stopping");
    assert_eq!(r.stops, 1, "{what}: stopped by the stall guard");
}

#[tokio::test]
async fn identical_retrieval_forever_is_stopped() {
    let r = run(looping(|_| {
        ("search_evidence".into(), json!({ "query": "latency" }))
    }))
    .await;
    assert_stopped(&r, "same search", 6);
}

#[tokio::test]
async fn oscillating_between_two_searches_is_stopped() {
    let r = run(looping(|i| {
        let q = if i % 2 == 0 { "latency" } else { "timeouts" };
        ("search_evidence".into(), json!({ "query": q }))
    }))
    .await;
    assert_stopped(&r, "a/b oscillation", 7);
}

#[tokio::test]
async fn repeating_one_hypothesis_is_stopped() {
    let r = run(looping(|_| {
        (
            "record_state".into(),
            json!({ "kind": "hypothesis", "text": "the cache is cold" }),
        )
    }))
    .await;
    assert_stopped(&r, "same hypothesis", 6);
}

#[tokio::test]
async fn retrying_a_failing_call_forever_is_stopped() {
    let r = run(looping(|_| ("svc.down".into(), json!({ "key": "a" })))).await;
    assert_stopped(&r, "same failing call", 6);
}

#[tokio::test]
async fn varied_real_work_is_not_cut_off() {
    // Ten different reads: every step brings new evidence, so the guard never fires and the
    // run is bounded only by the step limit.
    let r = run(looping(|i| {
        ("svc.read".into(), json!({ "key": format!("k{i}") }))
    }))
    .await;
    assert_eq!(r.nudges, 0, "no nudge while every step is new");
    assert_eq!(r.stops, 0);
    assert_eq!(r.steps, 16, "ran to the step limit");
}

fn say(text: &str) -> Completion {
    Completion {
        content: text.into(),
        tool_calls: vec![],
        usage: Usage::measured(10, 5),
    }
}

async fn answer_after(script: Vec<Completion>) -> (String, Arc<MockProvider>) {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open_with(dir.path(), Registry::new().with(Arc::new(Svc))).unwrap();
    let project = engine.create_project("t", "").unwrap();
    let model = Arc::new(MockProvider::new("m", script));
    let p: Arc<dyn Provider> = model.clone();
    let out = Harness::new(ModelRouter::single(p))
        .planner(false)
        .critic(false)
        .ask(
            &engine,
            &AgentProfile::default(),
            &project.id,
            "what is the total",
            |_| {},
        )
        .await
        .unwrap();
    (out.answer, model)
}

#[tokio::test]
async fn a_tool_call_written_as_text_is_not_taken_as_the_answer() {
    let (answer, model) = answer_after(vec![
        say(r#"record_state({ "kind": "finding", "text": "the total is 5" })"#),
        say("The total is 5."),
    ])
    .await;
    assert_eq!(answer, "The total is 5.");
    let second = &model.recorded()[1];
    assert!(
        second
            .messages
            .iter()
            .any(|m| m.content.contains("wrote a tool call as text")),
        "the model is told why"
    );
}

#[tokio::test]
async fn the_text_call_correction_happens_once() {
    let (answer, model) = answer_after(vec![
        say(r#"record_state({ "kind": "finding" })"#),
        say(r#"record_state({ "kind": "finding" }) again"#),
    ])
    .await;
    assert!(
        answer.contains("again"),
        "the second reply is accepted: {answer}"
    );
    assert_eq!(model.call_count(), 2);
}
