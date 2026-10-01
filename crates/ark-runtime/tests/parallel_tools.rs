//! Independent reads from one model reply run together; results still come back in call
//! order, a state change splits the batch, identical calls run once, and cancelling stops
//! the whole batch.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ark_core::{ActionDescriptor, RiskTier};
use ark_model::{Completion, Message, MockProvider, Provider, Role, ToolCall, Usage};
use ark_runtime::{
    AgentProfile, ApprovalDecision, ApprovalGate, CancelToken, Engine, FixedApprover, Harness,
    HarnessEvent, ModelRouter,
};
use ark_tools::{ActionOutput, Adapter, Registry, RunCtx};
use serde_json::{json, Value};

/// `slow.read` sleeps 200 ms and records when it started and finished; `slow.write` records
/// its turn in the same log.
struct Slow {
    log: Arc<Mutex<Vec<String>>>,
    runs: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl Adapter for Slow {
    fn name(&self) -> &str {
        "slow"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![
            ActionDescriptor::new("slow.read", "a slow read", RiskTier::Read),
            ActionDescriptor::new("slow.write", "a change", RiskTier::Mutate),
        ]
    }
    async fn run(
        &self,
        action: &str,
        input: Value,
        _c: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        let k = input["k"].as_str().unwrap_or("").to_string();
        self.log.lock().unwrap().push(format!("start {action} {k}"));
        if action == "slow.read" {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        self.log.lock().unwrap().push(format!("end {action} {k}"));
        Ok(ActionOutput::json(json!({ "k": k })))
    }
}

fn reply(calls: &[(&str, &str)]) -> Completion {
    Completion {
        content: String::new(),
        tool_calls: calls
            .iter()
            .enumerate()
            .map(|(i, (name, k))| ToolCall {
                id: format!("c{i}"),
                name: name.to_string(),
                arguments: json!({ "k": k }),
            })
            .collect(),
        usage: Usage::measured(10, 5),
    }
}

fn done() -> Completion {
    Completion {
        content: "done".into(),
        tool_calls: vec![],
        usage: Usage::measured(10, 5),
    }
}

struct Out {
    ms: u128,
    log: Vec<String>,
    results: Vec<String>,
    dups: usize,
    runs: usize,
    model: Arc<MockProvider>,
}

async fn run(calls: &[(&str, &str)], cancel_after: Option<Duration>) -> Out {
    let log = Arc::new(Mutex::new(Vec::new()));
    let runs = Arc::new(AtomicUsize::new(0));
    let dir = tempfile::tempdir().unwrap();
    let slow = Slow {
        log: log.clone(),
        runs: runs.clone(),
    };
    let engine = Engine::open_with(dir.path(), Registry::new().with(Arc::new(slow))).unwrap();
    let project = engine.create_project("t", "").unwrap().id;
    let model = Arc::new(MockProvider::new("m", vec![reply(calls), done()]));
    let p: Arc<dyn Provider> = model.clone();
    let token = CancelToken::new();
    if let Some(after) = cancel_after {
        let t = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(after).await;
            t.cancel();
        });
    }
    let gate = Arc::new(ApprovalGate::new(Arc::new(FixedApprover(
        ApprovalDecision::AllowOnce,
    ))));
    let profile = AgentProfile::builder()
        .core_actions(["slow.read", "slow.write"])
        .build();
    let mut results = Vec::new();
    let mut dups = 0;
    let started = Instant::now();
    Harness::new(ModelRouter::single(p))
        .planner(false)
        .critic(false)
        .approvals(Some(gate))
        .cancel(Some(token))
        .ask(&engine, &profile, &project, "read them", |e| match e {
            HarnessEvent::ToolResult { id, .. } => results.push(id),
            HarnessEvent::Duplicate { .. } => dups += 1,
            _ => {}
        })
        .await
        .unwrap();
    let logged = log.lock().unwrap().clone();
    Out {
        ms: started.elapsed().as_millis(),
        log: logged,
        results,
        dups,
        runs: runs.load(Ordering::SeqCst),
        model,
    }
}

/// The tool-result messages of the second request, as call ids.
fn result_ids(model: &MockProvider) -> Vec<String> {
    let recorded = model.recorded();
    recorded[1]
        .messages
        .iter()
        .filter(|m: &&Message| m.role == Role::Tool)
        .filter_map(|m| m.tool_call_id.clone())
        .collect()
}

#[tokio::test]
async fn independent_reads_run_together_in_call_order() {
    let o = run(
        &[("slow.read", "a"), ("slow.read", "b"), ("slow.read", "c")],
        None,
    )
    .await;
    assert!(o.ms < 500, "three 200 ms reads took {} ms", o.ms);
    assert_eq!(o.results, vec!["c0", "c1", "c2"], "events in call order");
    assert_eq!(
        result_ids(&o.model),
        vec!["c0", "c1", "c2"],
        "messages in call order"
    );
    let starts = o
        .log
        .iter()
        .take(3)
        .filter(|l| l.starts_with("start"))
        .count();
    assert_eq!(
        starts, 3,
        "all three started before any finished: {:?}",
        o.log
    );
}

#[tokio::test]
async fn a_change_splits_the_batch() {
    let o = run(
        &[("slow.read", "a"), ("slow.write", "w"), ("slow.read", "b")],
        None,
    )
    .await;
    let pos = |s: &str| o.log.iter().position(|l| l == s).unwrap();
    assert!(
        pos("end slow.read a") < pos("start slow.write w"),
        "{:?}",
        o.log
    );
    assert!(
        pos("end slow.write w") < pos("start slow.read b"),
        "{:?}",
        o.log
    );
    assert_eq!(result_ids(&o.model), vec!["c0", "c1", "c2"]);
}

#[tokio::test]
async fn identical_calls_in_one_reply_run_once() {
    let o = run(&[("slow.read", "a"), ("slow.read", "a")], None).await;
    assert_eq!(o.runs, 1);
    assert_eq!(o.dups, 1);
    assert_eq!(result_ids(&o.model).len(), 2, "both calls get a result");
}

#[tokio::test]
async fn cancelling_stops_the_whole_batch() {
    let o = run(
        &[("slow.read", "a"), ("slow.read", "b")],
        Some(Duration::from_millis(50)),
    )
    .await;
    assert!(
        o.results.is_empty(),
        "no result is recorded for a cancelled batch"
    );
    assert!(
        o.log.iter().all(|l| l.starts_with("start")),
        "nothing finished: {:?}",
        o.log
    );
    assert!(o.ms < 200, "returned at the cancel, not after the reads");
}
