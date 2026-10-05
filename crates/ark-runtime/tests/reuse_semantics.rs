//! When an earlier result may stand in for a new call (`Reuse`): never after a state change in
//! the same turn, never for a volatile action, across turns only for stable actions and reads
//! of stored artifacts, and a short artifact id names the same call as the full id.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use ark_core::{ActionDescriptor, ArtifactKind, Provenance, RiskTier};
use ark_model::{Completion, MockProvider, Provider, ToolCall, Usage};
use ark_runtime::{
    AgentProfile, ApprovalDecision, ApprovalGate, Engine, FixedApprover, Harness, HarnessEvent,
    ModelRouter,
};
use ark_tools::{ActionOutput, Adapter, Registry, RunCtx};
use serde_json::{json, Value};

/// `kv.get` reads a value that `kv.set` changes; `kv.sample` is volatile; `kv.parse` reads a
/// stored artifact; `kv.peek` observes a live system and also takes an artifact.
struct Kv {
    value: std::sync::Mutex<String>,
    runs: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl Adapter for Kv {
    fn name(&self) -> &str {
        "kv"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![
            ActionDescriptor::new("kv.get", "read the value", RiskTier::Read),
            ActionDescriptor::new("kv.set", "change the value", RiskTier::Mutate),
            ActionDescriptor::new("kv.sample", "sample a live counter", RiskTier::Observe)
                .volatile(),
            ActionDescriptor::new("kv.parse", "parse a stored file", RiskTier::Read),
            ActionDescriptor::new("kv.peek", "observe a device for a file", RiskTier::Observe),
        ]
    }
    async fn run(
        &self,
        action: &str,
        input: Value,
        _c: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        if action == "kv.set" {
            *self.value.lock().unwrap() = input["value"].as_str().unwrap_or("").to_string();
        }
        Ok(ActionOutput::json(
            json!({ "value": *self.value.lock().unwrap() }),
        ))
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    engine: Engine,
    project: ark_core::ProjectId,
    runs: Arc<AtomicUsize>,
}

fn fixture() -> Fixture {
    let runs = Arc::new(AtomicUsize::new(0));
    let dir = tempfile::tempdir().unwrap();
    let kv = Kv {
        value: std::sync::Mutex::new("slow".into()),
        runs: runs.clone(),
    };
    let engine = Engine::open_with(dir.path(), Registry::new().with(Arc::new(kv))).unwrap();
    let project = engine.create_project("t", "").unwrap().id;
    Fixture {
        _dir: dir,
        engine,
        project,
        runs,
    }
}

fn calls(steps: Vec<(&str, Value)>, end: &str) -> Vec<Completion> {
    let mut v: Vec<Completion> = steps
        .into_iter()
        .enumerate()
        .map(|(i, (name, arguments))| Completion {
            content: String::new(),
            tool_calls: vec![ToolCall {
                id: format!("c{i}"),
                name: name.into(),
                arguments,
            }],
            usage: Usage::measured(10, 5),
        })
        .collect();
    v.push(Completion {
        content: end.into(),
        tool_calls: vec![],
        usage: Usage::measured(10, 5),
    });
    v
}

/// Run one turn; returns (values the reads returned, duplicates served).
async fn turn(f: &Fixture, script: Vec<Completion>) -> (Vec<String>, usize) {
    let p: Arc<dyn Provider> = Arc::new(MockProvider::new("m", script));
    let profile = AgentProfile::builder()
        .core_actions(["kv.get", "kv.set", "kv.sample", "kv.parse", "kv.peek"])
        .build();
    let gate = Arc::new(ApprovalGate::new(Arc::new(FixedApprover(
        ApprovalDecision::AllowOnce,
    ))));
    let mut values = Vec::new();
    let mut dups = 0;
    Harness::new(ModelRouter::single(p))
        .planner(false)
        .critic(false)
        .approvals(Some(gate))
        .ask(
            &f.engine,
            &profile,
            &f.project,
            "check the value",
            |e| match e {
                HarnessEvent::ToolResult {
                    action, summary, ..
                } if action != "kv.set" => values.push(summary),
                HarnessEvent::Duplicate { .. } => dups += 1,
                _ => {}
            },
        )
        .await
        .unwrap();
    (values, dups)
}

#[tokio::test]
async fn a_read_after_a_write_runs_again() {
    let f = fixture();
    let (values, dups) = turn(
        &f,
        calls(
            vec![
                ("kv.get", json!({ "key": "mode" })),
                ("kv.set", json!({ "key": "mode", "value": "fast" })),
                ("kv.get", json!({ "key": "mode" })),
            ],
            "done",
        ),
    )
    .await;
    assert_eq!(
        dups, 0,
        "the second read must not reuse the stale first one"
    );
    assert_eq!(values.len(), 2);
    assert!(
        values[0].contains("slow") && values[1].contains("fast"),
        "{values:?}"
    );
}

#[tokio::test]
async fn reads_are_still_reused_until_something_changes() {
    let f = fixture();
    let (_, dups) = turn(
        &f,
        calls(
            vec![
                ("kv.get", json!({ "key": "mode" })),
                ("kv.get", json!({ "key": "mode" })),
            ],
            "done",
        ),
    )
    .await;
    assert_eq!(dups, 1);
    assert_eq!(f.runs.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_volatile_action_always_runs() {
    let f = fixture();
    let (values, dups) = turn(
        &f,
        calls(
            vec![("kv.sample", json!({})), ("kv.sample", json!({}))],
            "done",
        ),
    )
    .await;
    assert_eq!(dups, 0);
    assert_eq!(values.len(), 2);
}

#[tokio::test]
async fn a_stored_artifact_read_carries_across_turns_but_an_observation_does_not() {
    let f = fixture();
    let art = f
        .engine
        .ingest_bytes(
            &f.project,
            "a.bin",
            ArtifactKind::Blob,
            b"bytes",
            Provenance::Ingested {
                path: "a.bin".into(),
            },
        )
        .unwrap();
    let full = art.id.as_str().to_string();
    let short = format!("art:{}", art.id.short());
    // Turn 1 runs both with the full id.
    turn(
        &f,
        calls(
            vec![
                ("kv.parse", json!({ "artifact": full })),
                ("kv.peek", json!({ "artifact": full })),
            ],
            "done",
        ),
    )
    .await;
    assert_eq!(f.runs.load(Ordering::SeqCst), 2);
    // Turn 2 asks again with the short id: the parse is reused, the observation runs.
    let (values, dups) = turn(
        &f,
        calls(
            vec![
                ("kv.parse", json!({ "artifact": short })),
                ("kv.peek", json!({ "artifact": short })),
            ],
            "done",
        ),
    )
    .await;
    assert_eq!(dups, 1, "the short id names the same stored-artifact read");
    assert_eq!(values.len(), 1, "only the observation ran");
    assert_eq!(f.runs.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn an_identical_change_runs_at_most_twice_in_a_task() {
    let f = fixture();
    let set = || ("kv.set", json!({ "key": "mode", "value": "fast" }));
    let script = calls(
        vec![
            set(),
            set(),
            set(),
            set(),
            ("kv.set", json!({ "key": "mode", "value": "slow" })),
        ],
        "done",
    );
    let p: Arc<dyn Provider> = Arc::new(MockProvider::new("m", script));
    let profile = AgentProfile::builder().core_actions(["kv.set"]).build();
    let gate = Arc::new(ApprovalGate::new(Arc::new(FixedApprover(
        ApprovalDecision::AllowOnce,
    ))));
    let mut asked = 0;
    let out = Harness::new(ModelRouter::single(p))
        .planner(false)
        .critic(false)
        .approvals(Some(gate))
        .ask(&f.engine, &profile, &f.project, "set the mode", |e| {
            if let HarnessEvent::ToolCall { .. } = e {
                asked += 1;
            }
        })
        .await
        .unwrap();
    // Twice the same, refused twice without being run or asked about, then a different one.
    assert_eq!(f.runs.load(Ordering::SeqCst), 3);
    assert_eq!(asked, 3);
    assert_eq!(out.answer, "done");
}
