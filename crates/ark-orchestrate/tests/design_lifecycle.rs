use ark_core::{ActionDescriptor, RiskTier};
use ark_model::{Completion, Message, Provider, ProviderError, ToolCall, ToolDef, Usage};
use ark_orchestrate::{AgentSpec, Orchestrator, TaskSpec, TaskState};
use ark_sdk::prelude::Ark;
use ark_tools::{ActionOutput, Adapter, RunCtx, TurnLifecycle};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

struct RetryModel(AtomicUsize);
#[async_trait::async_trait]
impl Provider for RetryModel {
    async fn complete(
        &self,
        _: &str,
        _: &[Message],
        _: &[ToolDef],
    ) -> ark_model::Result<Completion> {
        match self.0.fetch_add(1, Ordering::SeqCst) {
            0 | 2 => Ok(Completion {
                content: String::new(),
                tool_calls: vec![ToolCall {
                    id: "call".into(),
                    name: "lifecycle.read".into(),
                    arguments: json!({}),
                }],
                usage: Usage::measured(1, 1),
            }),
            1 => Err(ProviderError::Api {
                provider: "fixture".into(),
                detail: "first attempt failed".into(),
            }),
            _ => Ok(Completion {
                content: "Done".into(),
                tool_calls: vec![],
                usage: Usage::measured(1, 1),
            }),
        }
    }
    fn describe(&self) -> String {
        "fixture".into()
    }
}
struct LifecycleTool {
    ids: Arc<Mutex<Vec<String>>>,
    finishes: Arc<AtomicUsize>,
}
#[async_trait::async_trait]
impl Adapter for LifecycleTool {
    fn name(&self) -> &str {
        "lifecycle"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![ActionDescriptor::new(
            "lifecycle.read",
            "Read fixture",
            RiskTier::Read,
        )]
    }
    async fn run(&self, _: &str, _: Value, ctx: &RunCtx<'_>) -> ark_tools::Result<ActionOutput> {
        let fence = ctx.commit_fence.clone().expect("attempt fence");
        self.ids.lock().unwrap().push(fence.turn_id().into());
        let finishes = self.finishes.clone();
        tokio::task::spawn_blocking(move || {
            let binding = Arc::new(TurnLifecycle {
                key: "fixture".into(),
                initialize: Arc::new(|| Ok(())),
                on_stop: Arc::new(|| Ok(())),
                on_finish: Arc::new(move || {
                    finishes.fetch_add(1, Ordering::SeqCst);
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
async fn failed_attempt_gets_a_fresh_lifecycle_fence_on_retry() {
    let root = tempfile::tempdir().unwrap();
    let ids = Arc::new(Mutex::new(Vec::new()));
    let finishes = Arc::new(AtomicUsize::new(0));
    let model: Arc<dyn Provider> = Arc::new(RetryModel(AtomicUsize::new(0)));
    let ark = Ark::builder()
        .store_root(root.path())
        .provider(model.clone())
        .tool(Arc::new(LifecycleTool {
            ids: ids.clone(),
            finishes: finishes.clone(),
        }))
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.create_project("lifecycle", "fixture").unwrap();
    let orchestrator = Orchestrator::builder(&ark, &project)
        .configure_models(move |b| b.provider("fixture", model))
        .build()
        .unwrap();
    orchestrator
        .spawn(
            AgentSpec::new("worker")
                .model("fixture")
                .tools(["lifecycle.*"]),
        )
        .await
        .unwrap();
    let task = orchestrator
        .run(
            "worker",
            TaskSpec::new("Read the fixture").attempts(2).retry_after(1),
        )
        .await
        .unwrap();
    assert_eq!(task.state, TaskState::Done, "{:?}", task.error);
    assert_eq!(task.attempts, 2);
    let ids = ids.lock().unwrap();
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1]);
    assert_eq!(finishes.load(Ordering::SeqCst), 2);
}

struct WaitingModel(AtomicUsize);
#[async_trait::async_trait]
impl Provider for WaitingModel {
    async fn complete(
        &self,
        _: &str,
        _: &[Message],
        _: &[ToolDef],
    ) -> ark_model::Result<Completion> {
        if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
            Ok(Completion {
                content: String::new(),
                tool_calls: vec![ToolCall {
                    id: "call".into(),
                    name: "pending.read".into(),
                    arguments: json!({}),
                }],
                usage: Usage::measured(1, 1),
            })
        } else {
            std::future::pending().await
        }
    }
    fn describe(&self) -> String {
        "waiting".into()
    }
}
struct PendingTool {
    ready: Arc<tokio::sync::Notify>,
    calls: Arc<AtomicUsize>,
    release: Arc<Mutex<std::sync::mpsc::Receiver<()>>>,
}
#[async_trait::async_trait]
impl Adapter for PendingTool {
    fn name(&self) -> &str {
        "pending"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![ActionDescriptor::new(
            "pending.read",
            "Read fixture",
            RiskTier::Read,
        )]
    }
    async fn run(&self, _: &str, _: Value, ctx: &RunCtx<'_>) -> ark_tools::Result<ActionOutput> {
        let fence = ctx.commit_fence.clone().unwrap();
        let ready = self.ready.clone();
        let calls = self.calls.clone();
        let release = self.release.clone();
        tokio::task::spawn_blocking(move || {
            let binding = Arc::new(TurnLifecycle {
                key: "pending".into(),
                initialize: Arc::new(|| Ok(())),
                on_finish: Arc::new(|| Ok(())),
                on_stop: Arc::new(move || {
                    if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                        return Err("injected cleanup failure".into());
                    }
                    release
                        .lock()
                        .unwrap()
                        .recv()
                        .map_err(|error| error.to_string())?;
                    Ok(())
                }),
            });
            assert!(fence.register_lifecycle(binding).unwrap());
            ready.notify_one();
        })
        .await
        .unwrap();
        Ok(ActionOutput::json(json!({})))
    }
}

#[tokio::test]
async fn task_stop_waits_for_failed_then_retried_lifecycle_cleanup() {
    let root = tempfile::tempdir().unwrap();
    let ready = Arc::new(tokio::sync::Notify::new());
    let calls = Arc::new(AtomicUsize::new(0));
    let (release, receive) = std::sync::mpsc::channel();
    let model: Arc<dyn Provider> = Arc::new(WaitingModel(AtomicUsize::new(0)));
    let ark = Ark::builder()
        .store_root(root.path())
        .provider(model.clone())
        .tool(Arc::new(PendingTool {
            ready: ready.clone(),
            calls: calls.clone(),
            release: Arc::new(Mutex::new(receive)),
        }))
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.create_project("stop lifecycle", "fixture").unwrap();
    let orchestrator = Orchestrator::builder(&ark, &project)
        .configure_models(move |b| b.provider("fixture", model))
        .build()
        .unwrap();
    orchestrator
        .spawn(
            AgentSpec::new("worker")
                .model("fixture")
                .tools(["pending.*"]),
        )
        .await
        .unwrap();
    let task = orchestrator
        .submit("worker", TaskSpec::new("Read the fixture"))
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), ready.notified())
        .await
        .unwrap();
    let mut stop = Box::pin(orchestrator.cancel_and_wait(&task));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), stop.as_mut())
            .await
            .is_err()
    );
    assert!(!orchestrator.task(&task).unwrap().state.is_final());
    release.send(()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), stop)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        orchestrator.task(&task).unwrap().state,
        TaskState::Cancelled
    );
    assert!(calls.load(Ordering::SeqCst) >= 2);
}

struct DelegateModel {
    to: &'static str,
    calls: AtomicUsize,
}
#[async_trait::async_trait]
impl Provider for DelegateModel {
    async fn complete(
        &self,
        _: &str,
        _: &[Message],
        _: &[ToolDef],
    ) -> ark_model::Result<Completion> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            Ok(Completion {
                content: String::new(),
                tool_calls: vec![ToolCall {
                    id: "delegate".into(),
                    name: "agents.delegate".into(),
                    arguments: json!({"to":self.to,"objective":"Continue the fixture"}),
                }],
                usage: Usage::measured(1, 1),
            })
        } else {
            std::future::pending().await
        }
    }
    fn describe(&self) -> String {
        "delegate".into()
    }
}

#[tokio::test]
async fn tree_stop_waits_for_a_grandchild_lifecycle() {
    let root = tempfile::tempdir().unwrap();
    let ready = Arc::new(tokio::sync::Notify::new());
    let calls = Arc::new(AtomicUsize::new(0));
    let (release, receive) = std::sync::mpsc::channel();
    let lead: Arc<dyn Provider> = Arc::new(DelegateModel {
        to: "middle",
        calls: AtomicUsize::new(0),
    });
    let middle: Arc<dyn Provider> = Arc::new(DelegateModel {
        to: "worker",
        calls: AtomicUsize::new(0),
    });
    let worker: Arc<dyn Provider> = Arc::new(WaitingModel(AtomicUsize::new(0)));
    let ark = Ark::builder()
        .store_root(root.path())
        .provider(lead.clone())
        .tool(Arc::new(PendingTool {
            ready: ready.clone(),
            calls: calls.clone(),
            release: Arc::new(Mutex::new(receive)),
        }))
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.create_project("tree lifecycle", "fixture").unwrap();
    let orchestrator = Orchestrator::builder(&ark, &project)
        .configure_models(move |b| {
            b.provider("lead", lead)
                .provider("middle", middle)
                .provider("worker", worker)
        })
        .build()
        .unwrap();
    orchestrator
        .spawn(
            AgentSpec::new("lead")
                .model("lead")
                .delegates_to(["middle"]),
        )
        .await
        .unwrap();
    orchestrator
        .spawn(
            AgentSpec::new("middle")
                .model("middle")
                .delegates_to(["worker"]),
        )
        .await
        .unwrap();
    orchestrator
        .spawn(
            AgentSpec::new("worker")
                .model("worker")
                .tools(["pending.*"]),
        )
        .await
        .unwrap();
    let task = orchestrator
        .submit("lead", TaskSpec::new("Run the fixture"))
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), ready.notified())
        .await
        .unwrap();
    let child = orchestrator.task(&task).unwrap().children[0].clone();
    let grandchild = orchestrator.task(&child).unwrap().children[0].clone();
    let mut stop = Box::pin(orchestrator.cancel_and_wait(&task));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), stop.as_mut())
            .await
            .is_err()
    );
    assert!(!orchestrator.task(&grandchild).unwrap().state.is_final());
    release.send(()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), stop)
        .await
        .unwrap()
        .unwrap();
    for id in [&task, &child, &grandchild] {
        assert!(orchestrator.task(id).unwrap().state.is_final());
    }
    assert!(calls.load(Ordering::SeqCst) >= 2);
}
