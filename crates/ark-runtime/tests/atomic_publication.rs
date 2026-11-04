use std::sync::Arc;

use ark_core::{
    ActionDescriptor, Artifact, ArtifactKind, InvocationStatus, ProjectId, Provenance, RiskTier,
};
use ark_runtime::{CancelToken, Engine};
use ark_store::{
    OutputPublicationCommit, RecordCompareGuards, RecordCompareLimits, RecordMutationEpoch,
    RecordUpdate, Store,
};
use ark_tools::{ActionOutput, Adapter, AdapterError, Registry, RunCtx};
use serde_json::{json, Value};

struct Publisher(Arc<Store>, Option<Arc<Gate>>, Option<ProjectId>);

struct Gate {
    inside: bool,
    entered: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<String>>>,
    proceed: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
    finished: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
}

impl Gate {
    fn pause(&self, invocation: &str) {
        self.entered
            .lock()
            .unwrap()
            .take()
            .unwrap()
            .send(invocation.to_owned())
            .unwrap();
        self.proceed
            .lock()
            .unwrap()
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
    }
}

struct SignalFinished(Option<tokio::sync::oneshot::Sender<()>>);
impl Drop for SignalFinished {
    fn drop(&mut self) {
        if let Some(done) = self.0.take() {
            let _ = done.send(());
        }
    }
}

#[async_trait::async_trait]
impl Adapter for Publisher {
    fn name(&self) -> &str {
        "publication_fixture"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![ActionDescriptor::new(
            "publication_fixture.write",
            "Publish a fixture",
            RiskTier::Mutate,
        )]
    }
    async fn run(
        &self,
        _: &str,
        input: Value,
        ctx: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        ctx.request_atomic_publication()?;
        if input["mode"] == "missing" {
            return Ok(ActionOutput::json(json!({"claimed": true})));
        }
        let project = ProjectId(ctx.project.as_ref().unwrap().clone());
        let owner = self.2.clone().unwrap_or_else(|| project.clone());
        let reparent = input
            .get("reparent")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let invocation_id = ctx.invocation.as_ref().unwrap().clone();
        let store = self.0.clone();
        let gate = self.1.clone();
        let fence = ctx
            .commit_fence
            .clone()
            .ok_or_else(|| AdapterError::Tool("Missing fence".into()))?;
        tokio::task::spawn_blocking(move || -> ark_tools::Result<()> {
            let _finished = SignalFinished(
                gate.as_ref()
                    .and_then(|gate| gate.finished.lock().unwrap().take()),
            );
            let running = store
                .get_invocation(&project, &invocation_id)
                .map_err(|e| AdapterError::Tool(e.to_string()))?
                .unwrap();
            let blob = store
                .put_blob(b"runtime-output")
                .map_err(|e| AdapterError::Tool(e.to_string()))?;
            let artifact = Artifact::new(
                blob.clone(),
                ArtifactKind::Binary,
                "output",
                14,
                Provenance::Produced {
                    invocation: running.id.clone(),
                },
            );
            let mut finished = running.clone();
            finished.produced.push(blob);
            finished.mark_ok(json!({"published": true}));
            let prepared = store
                .prepare_output_publication(&project, &running, &finished, &[artifact])
                .map_err(|e| AdapterError::Tool(e.to_string()))?;
            let receipt = json!({"invocation": invocation_id});
            let updates = [RecordUpdate {
                kind: "fixture.receipt",
                id: &invocation_id,
                scope: owner.as_str(),
                expected: None,
                value: &receipt,
            }];
            let kinds = ["fixture.authority"];
            let epoch = store
                .watched_record_epoch(owner.as_str(), &kinds)
                .map_err(|e| AdapterError::Tool(e.to_string()))?;
            if let Some(gate) = gate.as_ref().filter(|gate| !gate.inside) {
                gate.pause(&invocation_id);
            }
            let mut result = None;
            let ran = fence.if_active(&mut || {
                if let Some(gate) = gate.as_ref().filter(|gate| gate.inside) {
                    gate.pause(&invocation_id);
                }
                result = Some(store.compare_put_records_with_publication(
                    &updates,
                    &[],
                    &[],
                    &[],
                    OutputPublicationCommit {
                        limits: RecordCompareLimits {
                            max_bytes: 8192,
                            max_total_bytes: 32768,
                            max_records: 8,
                        },
                        guards: RecordCompareGuards {
                            immutable_epoch: None,
                            record_epoch: RecordMutationEpoch {
                                scope: owner.as_str(),
                                kinds: &kinds,
                                value: epoch,
                            },
                        },
                        publication: &prepared,
                        receipt_keys: &[("fixture.receipt", &invocation_id)],
                    },
                ));
            });
            if !ran {
                return Err(AdapterError::Tool("Stopped".into()));
            }
            if !result
                .unwrap()
                .map_err(|e| AdapterError::Tool(e.to_string()))?
            {
                return Err(AdapterError::Tool("Publication conflicted".into()));
            }
            if let Some(parent) = reparent {
                let mut child = store
                    .get_project(&project)
                    .map_err(|e| AdapterError::Tool(e.to_string()))?
                    .ok_or_else(|| AdapterError::Tool("Project missing".into()))?;
                child.parent = Some(ProjectId(parent));
                store
                    .put_project(&child)
                    .map_err(|e| AdapterError::Tool(e.to_string()))?;
            }
            Ok(())
        })
        .await
        .map_err(|e| AdapterError::Tool(e.to_string()))??;
        Err(AdapterError::Tool(
            "Lost acknowledgement after commit".into(),
        ))
    }
}

#[derive(Default)]
struct PublicationGuard(
    std::sync::Mutex<Vec<InvocationStatus>>,
    std::sync::atomic::AtomicUsize,
);
impl ark_runtime::guard::CallGuard for PublicationGuard {
    fn check(
        &self,
        _: &str,
        _: &Value,
        _: Option<&ActionDescriptor>,
    ) -> ark_runtime::guard::Verdict {
        ark_runtime::guard::Verdict::Allow
    }
    fn prepare(&self, _: &ProjectId, _: &ark_core::Invocation) -> Result<(), String> {
        Ok(())
    }
    fn started(&self, _: &ProjectId, _: &ark_core::Invocation) -> Result<(), String> {
        Ok(())
    }
    fn ended(&self, _: &ProjectId, invocation: &ark_core::Invocation) -> Result<(), String> {
        self.0.lock().unwrap().push(invocation.status);
        Ok(())
    }
    fn aborted(&self, _: &ProjectId, _: &ark_core::Invocation, _: bool) -> Result<(), String> {
        self.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn runtime_preserves_committed_result_and_refuses_uncommitted_success() {
    let root = tempfile::tempdir().unwrap();
    let mut engine = Engine::new(Store::open(root.path()).unwrap(), Registry::new());
    engine
        .registry
        .register(Arc::new(Publisher(engine.store.clone(), None, None)));
    let project = engine.create_project("publication", "").unwrap().id;
    engine
        .store
        .register_record_watch(project.as_str(), &["fixture.authority"])
        .unwrap();
    let pending = engine
        .run_action(&project, "publication_fixture.write", json!({}), false)
        .await
        .unwrap();
    assert_eq!(pending.status, InvocationStatus::PendingApproval);
    let guard = Arc::new(PublicationGuard::default());
    let engine = engine.guarded(guard.clone());
    let token = Arc::new(CancelToken::new());
    let committed = engine
        .run_action_with_fence(
            &project,
            "publication_fixture.write",
            json!({"mode":"commit"}),
            true,
            Some(token),
        )
        .await
        .unwrap();
    assert_eq!(committed.status, InvocationStatus::Ok);
    assert_eq!(committed.output, json!({"published": true}));
    let stored = engine
        .store
        .load_output_publication(&project, committed.id.as_str())
        .unwrap()
        .unwrap();
    assert_eq!(stored.invocation.output, committed.output);
    assert_eq!(stored.outputs[0].id, committed.produced[0]);
    assert_eq!(
        engine
            .store
            .read_blob_bounded(&committed.produced[0], 64)
            .unwrap(),
        b"runtime-output"
    );
    let missing = engine
        .run_action(
            &project,
            "publication_fixture.write",
            json!({"mode":"missing"}),
            true,
        )
        .await
        .unwrap();
    assert_eq!(missing.status, InvocationStatus::Failed);
    assert!(missing.produced.is_empty());
    let stopped = Arc::new(CancelToken::new());
    stopped.cancel();
    let failed = engine
        .run_action_with_fence(
            &project,
            "publication_fixture.write",
            json!({"mode":"stop"}),
            true,
            Some(stopped),
        )
        .await
        .unwrap();
    assert_eq!(failed.status, InvocationStatus::Failed);
    assert!(engine
        .store
        .load_output_publication(&project, failed.id.as_str())
        .unwrap()
        .is_none());
    assert_eq!(
        *guard.0.lock().unwrap(),
        vec![
            InvocationStatus::Ok,
            InvocationStatus::Failed,
            InvocationStatus::Failed
        ]
    );
    assert_eq!(guard.1.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test]
async fn stop_orders_a_detached_publication_on_both_sides_of_the_fence() {
    for inside in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let mut engine = Engine::new(Store::open(root.path()).unwrap(), Registry::new());
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (proceed_tx, proceed_rx) = std::sync::mpsc::channel();
        let (finished_tx, finished_rx) = tokio::sync::oneshot::channel();
        let gate = Arc::new(Gate {
            inside,
            entered: std::sync::Mutex::new(Some(entered_tx)),
            proceed: std::sync::Mutex::new(proceed_rx),
            finished: std::sync::Mutex::new(Some(finished_tx)),
        });
        engine
            .registry
            .register(Arc::new(Publisher(engine.store.clone(), Some(gate), None)));
        let project = engine.create_project("publication race", "").unwrap().id;
        engine
            .store
            .register_record_watch(project.as_str(), &["fixture.authority"])
            .unwrap();
        let engine = Arc::new(engine);
        let token = Arc::new(CancelToken::new());
        let worker_engine = engine.clone();
        let worker_project = project.clone();
        let worker_token = token.clone();
        let task = tokio::spawn(async move {
            worker_engine
                .run_action_with_fence(
                    &worker_project,
                    "publication_fixture.write",
                    json!({"mode":"race"}),
                    true,
                    Some(worker_token),
                )
                .await
        });
        let invocation_id = tokio::time::timeout(std::time::Duration::from_secs(10), entered_rx)
            .await
            .unwrap()
            .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        token.cancel();
        let stop_token = token.clone();
        let mut stop = tokio::spawn(async move { stop_token.wait_for_commit().await });
        if inside {
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(50), &mut stop)
                    .await
                    .is_err()
            );
        } else {
            tokio::time::timeout(std::time::Duration::from_secs(3), &mut stop)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        }
        proceed_tx.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), finished_rx)
            .await
            .unwrap()
            .unwrap();
        if inside {
            tokio::time::timeout(std::time::Duration::from_secs(3), &mut stop)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        }
        let publication = engine
            .store
            .load_output_publication(&project, &invocation_id)
            .unwrap();
        if inside {
            let publication = publication.unwrap();
            assert_eq!(publication.invocation.status, InvocationStatus::Ok);
            assert_eq!(
                engine
                    .store
                    .read_blob_bounded(&publication.outputs[0].id, 64)
                    .unwrap(),
                b"runtime-output"
            );
        } else {
            assert!(publication.is_none());
            assert!(engine
                .store
                .get_record("fixture.receipt", &invocation_id)
                .unwrap()
                .is_none());
        }
    }
}

#[tokio::test]
async fn harness_approval_controls_the_publication_path() {
    use ark_model::{Completion, MockProvider, Provider, ToolCall, Usage};
    use ark_runtime::{
        AgentProfile, ApprovalDecision, ApprovalGate, FixedApprover, Harness, HarnessEvent,
        ModelRouter,
    };
    for decision in [ApprovalDecision::Deny, ApprovalDecision::AllowOnce] {
        let root = tempfile::tempdir().unwrap();
        let mut engine = Engine::new(Store::open(root.path()).unwrap(), Registry::new());
        let owner = engine.create_project("owner", "").unwrap().id;
        let project = engine.create_project("approval", "").unwrap().id;
        let mut child = engine.store.get_project(&project).unwrap().unwrap();
        child.parent = Some(owner.clone());
        engine.store.put_project(&child).unwrap();
        engine.registry.register(Arc::new(Publisher(
            engine.store.clone(),
            None,
            Some(owner.clone()),
        )));
        engine
            .store
            .register_record_watch(owner.as_str(), &["fixture.authority"])
            .unwrap();
        let request = Completion {
            content: String::new(),
            tool_calls: vec![ToolCall {
                id: "call".into(),
                name: "publication_fixture.write".into(),
                arguments: json!({"mode":"commit"}),
            }],
            usage: Usage::measured(1, 1),
        };
        let done = Completion {
            content: "Done".into(),
            tool_calls: vec![],
            usage: Usage::measured(1, 1),
        };
        let model: Arc<dyn Provider> = Arc::new(MockProvider::new("fixture", vec![request, done]));
        let gate = Arc::new(ApprovalGate::new(Arc::new(FixedApprover(decision))));
        let profile = AgentProfile::builder()
            .core_actions(["publication_fixture.write"])
            .build();
        let mut resolved = Vec::new();
        Harness::new(ModelRouter::single(model))
            .planner(false)
            .critic(false)
            .approvals(Some(gate))
            .cancel(Some(CancelToken::new()))
            .ask(&engine, &profile, &project, "publish", |event| {
                if let HarnessEvent::ApprovalResolved { decision, .. } = event {
                    resolved.push(decision);
                }
            })
            .await
            .unwrap();
        assert_eq!(resolved, vec![decision]);
        let invocations = engine.store.list_invocations(&project).unwrap();
        if decision == ApprovalDecision::Deny {
            assert_eq!(invocations.len(), 1);
            assert_eq!(invocations[0].status, InvocationStatus::Denied);
            assert!(invocations[0].produced.is_empty());
            assert!(!engine
                .store
                .has_blob(&ark_core::ArtifactId::of_bytes(b"runtime-output")));
        } else {
            let invocation = invocations
                .iter()
                .find(|inv| inv.action == "publication_fixture.write")
                .unwrap();
            assert_eq!(invocation.status, InvocationStatus::Ok);
            let publication = engine
                .store
                .load_output_publication(&project, invocation.id.as_str())
                .unwrap()
                .unwrap();
            assert_eq!(publication.record_scope.as_deref(), Some(owner.as_str()));
            assert_eq!(publication.invocation.input, json!({"mode":"commit"}));
            assert_eq!(publication.outputs[0].id, invocation.produced[0]);
            let new_owner = engine.create_project("new owner", "").unwrap().id;
            let mut moved = engine.store.get_project(&project).unwrap().unwrap();
            moved.parent = Some(new_owner.clone());
            engine.store.put_project(&moved).unwrap();
            let historical = engine
                .store
                .load_output_publication(&project, invocation.id.as_str())
                .unwrap()
                .unwrap();
            assert_eq!(historical.invocation.id, invocation.id);
            assert_eq!(historical.outputs[0].id, invocation.produced[0]);
            assert!(historical.record_scope.is_none());
            assert!(historical.receipts.is_none());
            assert!(engine
                .store
                .load_output_publication(&new_owner, invocation.id.as_str())
                .unwrap()
                .is_none());
        }
    }
}

#[tokio::test]
async fn runtime_recovers_child_result_when_reparented_after_commit() {
    let root = tempfile::tempdir().unwrap();
    let mut engine = Engine::new(Store::open(root.path()).unwrap(), Registry::new());
    let owner = engine.create_project("owner", "").unwrap().id;
    let new_owner = engine.create_project("new owner", "").unwrap().id;
    let project = engine.create_project("child", "").unwrap().id;
    let mut child = engine.store.get_project(&project).unwrap().unwrap();
    child.parent = Some(owner.clone());
    engine.store.put_project(&child).unwrap();
    engine
        .store
        .register_record_watch(owner.as_str(), &["fixture.authority"])
        .unwrap();
    engine
        .registry
        .register(Arc::new(Publisher(engine.store.clone(), None, Some(owner))));
    let invocation = engine
        .run_action_with_fence(
            &project,
            "publication_fixture.write",
            json!({"mode":"commit","reparent":new_owner.as_str()}),
            true,
            Some(Arc::new(CancelToken::new())),
        )
        .await
        .unwrap();
    assert_eq!(invocation.status, InvocationStatus::Ok);
    let historical = engine
        .store
        .load_output_publication(&project, invocation.id.as_str())
        .unwrap()
        .unwrap();
    assert!(historical.record_scope.is_none());
    assert!(historical.receipts.is_none());
    assert_eq!(historical.invocation.id, invocation.id);
}
