use ark_core::{ActionDescriptor, RiskTier};
use ark_model::{Completion, MockProvider, Provider, ToolCall, Usage};
use ark_runtime::{
    AgentProfile, ApprovalDecision, ApprovalGate, CancelToken, Engine, FixedApprover, Harness,
    ModelRouter,
};
use ark_tools::{ActionOutput, Adapter, Registry, RunCtx};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tokio::sync::{oneshot, Notify};

struct BlockingMutation {
    fenced: bool,
    started: Arc<Notify>,
    release: std::sync::Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    committed: Arc<AtomicBool>,
    finished: std::sync::Mutex<Option<oneshot::Sender<()>>>,
}
#[async_trait::async_trait]
impl Adapter for BlockingMutation {
    fn name(&self) -> &str {
        "race"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![ActionDescriptor::new(
            "race.write",
            "write",
            RiskTier::Mutate,
        )]
    }
    async fn run(&self, _: &str, _: Value, ctx: &RunCtx<'_>) -> ark_tools::Result<ActionOutput> {
        let release = self.release.lock().unwrap().take().unwrap();
        let finished = self.finished.lock().unwrap().take().unwrap();
        let committed = self.committed.clone();
        let started = self.started.clone();
        let fence = if self.fenced {
            Some(ctx.commit_fence.clone().expect("turn fence"))
        } else {
            None
        };
        tokio::task::spawn_blocking(move || {
            started.notify_one();
            release.recv().unwrap();
            let mut commit = || committed.store(true, Ordering::SeqCst);
            if let Some(fence) = fence {
                fence.if_active(&mut commit);
            } else {
                commit();
            }
            let _ = finished.send(());
        })
        .await
        .unwrap();
        Ok(ActionOutput::json(json!({})))
    }
}
async fn exercise(fenced: bool) -> bool {
    let dir = tempfile::tempdir().unwrap();
    let started = Arc::new(Notify::new());
    let committed = Arc::new(AtomicBool::new(false));
    let (release, receive) = std::sync::mpsc::channel();
    let (finished, finished_rx) = oneshot::channel();
    let adapter = BlockingMutation {
        fenced,
        started: started.clone(),
        release: std::sync::Mutex::new(Some(receive)),
        committed: committed.clone(),
        finished: std::sync::Mutex::new(Some(finished)),
    };
    let engine = Engine::open_with(dir.path(), Registry::new().with(Arc::new(adapter))).unwrap();
    let project = engine.create_project("race", "").unwrap().id;
    let model: Arc<dyn Provider> = Arc::new(MockProvider::new(
        "m",
        vec![Completion {
            content: String::new(),
            tool_calls: vec![ToolCall {
                id: "c".into(),
                name: "race.write".into(),
                arguments: json!({}),
            }],
            usage: Usage::measured(1, 1),
        }],
    ));
    let token = CancelToken::new();
    let turn_token = token.clone();
    let task = tokio::spawn(async move {
        let gate = Arc::new(ApprovalGate::new(Arc::new(FixedApprover(
            ApprovalDecision::AllowOnce,
        ))));
        let profile = AgentProfile::builder().core_actions(["race.write"]).build();
        Harness::new(ModelRouter::single(model))
            .planner(false)
            .critic(false)
            .approvals(Some(gate))
            .cancel(Some(turn_token))
            .ask(&engine, &profile, &project, "write", |_| {})
            .await
            .unwrap()
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), started.notified())
        .await
        .unwrap();
    token.cancel(); // This is the operation the released Stop handler acknowledges.
    assert!(!committed.load(Ordering::SeqCst));
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(10), task)
        .await
        .unwrap()
        .unwrap();
    assert!(outcome.cancelled);
    release.send(()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), finished_rx)
        .await
        .unwrap()
        .unwrap();
    committed.load(Ordering::SeqCst)
}

#[tokio::test]
async fn released_stop_can_acknowledge_before_a_blocking_mutation_commits() {
    assert!(exercise(false).await);
}

#[tokio::test]
async fn fenced_commit_cannot_follow_stop_acknowledgement() {
    assert!(!exercise(true).await);
}

struct LifecycleAdapter {
    finished: Arc<AtomicBool>,
    expected_project: String,
}
#[async_trait::async_trait]
impl Adapter for LifecycleAdapter {
    fn name(&self) -> &str {
        "lifecycle"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![ActionDescriptor::new(
            "lifecycle.read",
            "read",
            RiskTier::Read,
        )]
    }
    async fn run(&self, _: &str, _: Value, ctx: &RunCtx<'_>) -> ark_tools::Result<ActionOutput> {
        assert_eq!(ctx.project.as_deref(), Some(self.expected_project.as_str()));
        let fence = ctx.commit_fence.clone().expect("turn fence");
        let finished = self.finished.clone();
        tokio::task::spawn_blocking(move || {
            let binding = Arc::new(ark_tools::TurnLifecycle {
                key: "lifecycle-test".into(),
                initialize: Arc::new(|| Ok(())),
                on_stop: Arc::new(|| Ok(())),
                on_finish: Arc::new(move || {
                    finished.store(true, Ordering::SeqCst);
                    Ok(())
                }),
            });
            assert!(fence.register_lifecycle(binding).unwrap());
        })
        .await
        .unwrap();
        Ok(ActionOutput::json(json!({})))
    }
}

#[tokio::test]
async fn shared_harness_finishes_consumer_lifecycle_without_a_ui_host() {
    let dir = tempfile::tempdir().unwrap();
    let initial = Engine::open(dir.path()).unwrap();
    let project = initial.create_project("lifecycle", "").unwrap().id;
    drop(initial);
    let finished = Arc::new(AtomicBool::new(false));
    let adapter = LifecycleAdapter {
        finished: finished.clone(),
        expected_project: project.as_str().into(),
    };
    let engine = Engine::open_with(dir.path(), Registry::new().with(Arc::new(adapter))).unwrap();
    let model: Arc<dyn Provider> = Arc::new(MockProvider::new(
        "m",
        vec![
            Completion {
                content: String::new(),
                tool_calls: vec![ToolCall {
                    id: "c".into(),
                    name: "lifecycle.read".into(),
                    arguments: json!({}),
                }],
                usage: Usage::measured(1, 1),
            },
            Completion {
                content: "Done".into(),
                tool_calls: vec![],
                usage: Usage::measured(1, 1),
            },
        ],
    ));
    let token = CancelToken::new();
    let profile = AgentProfile::builder()
        .core_actions(["lifecycle.read"])
        .build();
    Harness::new(ModelRouter::single(model))
        .planner(false)
        .critic(false)
        .cancel(Some(token.clone()))
        .ask(&engine, &profile, &project, "read", |_| {})
        .await
        .unwrap();
    assert!(finished.load(Ordering::SeqCst));
    assert!(!ark_tools::CommitFence::if_active(&token, &mut || panic!(
        "late action"
    )));
}

struct FailAfterLifecycle(std::sync::atomic::AtomicUsize);
#[async_trait::async_trait]
impl Provider for FailAfterLifecycle {
    async fn complete(
        &self,
        _: &str,
        _: &[ark_model::Message],
        _: &[ark_model::ToolDef],
    ) -> ark_model::Result<Completion> {
        if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
            Ok(Completion {
                content: String::new(),
                tool_calls: vec![ToolCall {
                    id: "c".into(),
                    name: "lifecycle.read".into(),
                    arguments: json!({}),
                }],
                usage: Usage::measured(1, 1),
            })
        } else {
            Err(ark_model::ProviderError::Api {
                provider: "fixture".into(),
                detail: "injected failure".into(),
            })
        }
    }
    fn describe(&self) -> String {
        "fixture".into()
    }
}

#[tokio::test]
async fn shared_harness_finishes_consumer_lifecycle_on_error() {
    let dir = tempfile::tempdir().unwrap();
    let initial = Engine::open(dir.path()).unwrap();
    let project = initial.create_project("lifecycle error", "").unwrap().id;
    drop(initial);
    let finished = Arc::new(AtomicBool::new(false));
    let adapter = LifecycleAdapter {
        finished: finished.clone(),
        expected_project: project.as_str().into(),
    };
    let engine = Engine::open_with(dir.path(), Registry::new().with(Arc::new(adapter))).unwrap();
    let model: Arc<dyn Provider> =
        Arc::new(FailAfterLifecycle(std::sync::atomic::AtomicUsize::new(0)));
    let token = CancelToken::new();
    let profile = AgentProfile::builder()
        .core_actions(["lifecycle.read"])
        .build();
    assert!(Harness::new(ModelRouter::single(model))
        .planner(false)
        .critic(false)
        .cancel(Some(token.clone()))
        .ask(&engine, &profile, &project, "read", |_| {})
        .await
        .is_err());
    assert!(finished.load(Ordering::SeqCst));
    assert!(!ark_tools::CommitFence::if_active(&token, &mut || panic!(
        "late action"
    )));
}
