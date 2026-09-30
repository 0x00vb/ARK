//! Dedup reuses a prior result only when that result succeeded. A failed or cancelled call is
//! not evidence: the same call must run again. All driven by the mock provider.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ark_core::{ActionDescriptor, RiskTier};
use ark_model::{Completion, MockProvider, Provider, ToolCall, Usage};
use ark_runtime::{AgentProfile, CancelToken, Engine, Harness, HarnessEvent, ModelRouter};
use ark_tools::{ActionOutput, Adapter, AdapterError, Registry, RunCtx};

/// `probe.check` fails for its first `fail_first` runs, then succeeds. `probe.slow` sleeps.
struct Probe {
    runs: Arc<AtomicUsize>,
    fail_first: usize,
}

#[async_trait::async_trait]
impl Adapter for Probe {
    fn name(&self) -> &str {
        "probe"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![
            ActionDescriptor::new("probe.check", "check the service", RiskTier::Read).reusable(),
            ActionDescriptor::new("probe.slow", "a slow read", RiskTier::Read).reusable(),
        ]
    }
    async fn run(
        &self,
        action: &str,
        _input: serde_json::Value,
        _c: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        let n = self.runs.fetch_add(1, Ordering::SeqCst);
        if action == "probe.slow" && n == 0 {
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
        if n < self.fail_first {
            return Err(AdapterError::bad_input(action, "service unavailable"));
        }
        Ok(ActionOutput::json(serde_json::json!({ "status": "up" })))
    }
}

fn setup(
    fail_first: usize,
) -> (
    tempfile::TempDir,
    Engine,
    ark_core::ProjectId,
    Arc<AtomicUsize>,
) {
    let runs = Arc::new(AtomicUsize::new(0));
    let dir = tempfile::tempdir().unwrap();
    let probe = Probe {
        runs: runs.clone(),
        fail_first,
    };
    let engine = Engine::open_with(dir.path(), Registry::new().with(Arc::new(probe))).unwrap();
    let p = engine.create_project("t", "").unwrap();
    (dir, engine, p.id, runs)
}

fn profile() -> AgentProfile {
    AgentProfile::builder()
        .core_actions(["probe.check", "probe.slow"])
        .build()
}

fn step(id: &str, action: &str) -> Completion {
    Completion {
        content: String::new(),
        tool_calls: vec![ToolCall {
            id: id.into(),
            name: action.into(),
            arguments: serde_json::json!({ "target": "api" }),
        }],
        usage: Usage::measured(100, 5),
    }
}

fn answer(text: &str) -> Completion {
    Completion {
        content: text.into(),
        tool_calls: vec![],
        usage: Usage::measured(100, 5),
    }
}

fn harness(script: Vec<Completion>) -> Harness {
    let p: Arc<dyn Provider> = Arc::new(MockProvider::new("m", script));
    Harness::new(ModelRouter::single(p))
        .planner(false)
        .critic(false)
}

#[derive(Default)]
struct Seen {
    results: Vec<bool>,
    duplicates: usize,
}

fn watch(seen: &mut Seen) -> impl FnMut(HarnessEvent) + '_ {
    |e| match e {
        HarnessEvent::ToolResult { ok, .. } => seen.results.push(ok),
        HarnessEvent::Duplicate { .. } => seen.duplicates += 1,
        _ => {}
    }
}

#[tokio::test]
async fn a_failed_call_runs_again_in_the_same_turn() {
    let (_d, engine, project, runs) = setup(1);
    let h = harness(vec![
        step("a", "probe.check"),
        step("b", "probe.check"),
        answer("up"),
    ]);
    let mut seen = Seen::default();
    h.ask(&engine, &profile(), &project, "is it up", watch(&mut seen))
        .await
        .unwrap();
    assert_eq!(
        runs.load(Ordering::SeqCst),
        2,
        "the retry must reach the adapter"
    );
    assert_eq!(seen.results, vec![false, true]);
    assert_eq!(
        seen.duplicates, 0,
        "a failure is never served as reused evidence"
    );
}

#[tokio::test]
async fn a_successful_call_is_still_reused() {
    let (_d, engine, project, runs) = setup(0);
    let h = harness(vec![
        step("a", "probe.check"),
        step("b", "probe.check"),
        answer("up"),
    ]);
    let mut seen = Seen::default();
    h.ask(&engine, &profile(), &project, "is it up", watch(&mut seen))
        .await
        .unwrap();
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    assert_eq!(seen.results, vec![true]);
    assert_eq!(seen.duplicates, 1);
}

#[tokio::test]
async fn a_failure_from_an_earlier_turn_is_not_reused() {
    let (_d, engine, project, runs) = setup(1);
    let first = harness(vec![step("a", "probe.check"), answer("down")]);
    first
        .ask(&engine, &profile(), &project, "is it up", |_| {})
        .await
        .unwrap();
    // `probe.check` is declared reusable, so a success would carry across turns. A failure
    // must not.
    let second = harness(vec![step("b", "probe.check"), answer("up")]);
    let mut seen = Seen::default();
    second
        .ask(&engine, &profile(), &project, "try again", watch(&mut seen))
        .await
        .unwrap();
    assert_eq!(runs.load(Ordering::SeqCst), 2);
    assert_eq!(seen.results, vec![true]);
    assert_eq!(seen.duplicates, 0);
}

#[tokio::test]
async fn a_cancelled_call_is_not_reused() {
    let (_d, engine, project, runs) = setup(0);
    let token = CancelToken::new();
    let first = harness(vec![step("a", "probe.slow"), answer("done")]).cancel(Some(token.clone()));
    let t2 = token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        t2.cancel();
    });
    let out = first
        .ask(&engine, &profile(), &project, "read it", |_| {})
        .await
        .unwrap();
    assert!(out.cancelled);

    let second = harness(vec![step("b", "probe.slow"), answer("done")]);
    let mut seen = Seen::default();
    second
        .ask(&engine, &profile(), &project, "read it", watch(&mut seen))
        .await
        .unwrap();
    assert_eq!(
        runs.load(Ordering::SeqCst),
        2,
        "the cancelled call runs again"
    );
    assert_eq!(seen.results, vec![true]);
    assert_eq!(seen.duplicates, 0);
}

#[tokio::test]
async fn repeating_a_failing_call_still_trips_the_stall_guard() {
    let (_d, engine, project, runs) = setup(usize::MAX);
    let p: Arc<dyn Provider> = Arc::new(MockProvider::new("m", vec![]).with_fallback(|r| {
        if r.tools.is_empty() {
            answer("it stays down")
        } else {
            step("x", "probe.check")
        }
    }));
    let h = Harness::new(ModelRouter::single(p))
        .planner(false)
        .critic(false)
        .max_steps(16);
    let out = h
        .ask(&engine, &profile(), &project, "is it up", |_| {})
        .await
        .unwrap();
    let n = runs.load(Ordering::SeqCst);
    assert!(
        n < 10,
        "stalled early instead of retrying for the whole budget ({n} runs)"
    );
    assert!(out.steps < 16);
}
