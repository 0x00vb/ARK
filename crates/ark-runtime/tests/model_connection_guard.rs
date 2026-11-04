use ark_core::{ActionDescriptor, Invocation, ProjectId};
use ark_model::{Completion, Message, Provider, ProviderError, ToolDef, Usage};
use ark_runtime::{
    guard::{CallGuard, Verdict},
    AgentProfile, Engine, Harness, ModelRouter,
};
use ark_store::Store;
use ark_tools::Registry;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

struct Model {
    revision: u64,
    rebound: Option<u64>,
    calls: Arc<AtomicUsize>,
    fail_once: bool,
}

#[async_trait::async_trait]
impl Provider for Model {
    fn host_controls_tools(&self) -> bool {
        true
    }
    fn connection_revision(&self) -> u64 {
        self.revision
    }
    fn model_id(&self) -> String {
        "fixture:model".into()
    }
    fn describe(&self) -> String {
        "fixture model".into()
    }
    fn for_conversation(&self, _: &str) -> ark_model::Result<Option<Arc<dyn Provider>>> {
        Ok(self.rebound.map(|revision| {
            Arc::new(Self {
                revision,
                rebound: None,
                calls: self.calls.clone(),
                fail_once: self.fail_once,
            }) as Arc<dyn Provider>
        }))
    }
    async fn complete(
        &self,
        _: &str,
        _: &[Message],
        _: &[ToolDef],
    ) -> ark_model::Result<Completion> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_once && call == 0 {
            return Err(ProviderError::Status {
                provider: "fixture".into(),
                status: 503,
                detail: "retry fixture".into(),
                retry_after: Some(std::time::Duration::ZERO),
            });
        }
        Ok(Completion {
            content: "ready".into(),
            tool_calls: vec![],
            usage: Usage::measured(10, 1),
        })
    }
}

struct Guard {
    expected: u64,
    checks: AtomicUsize,
    refuse_after: usize,
}
impl CallGuard for Guard {
    fn check_model(&self, model: &str, revision: u64) -> Result<(), String> {
        let call = self.checks.fetch_add(1, Ordering::SeqCst);
        if model != "fixture:model" || revision != self.expected || call >= self.refuse_after {
            Err("model connection changed".into())
        } else {
            Ok(())
        }
    }
    fn check(&self, _: &str, _: &serde_json::Value, _: Option<&ActionDescriptor>) -> Verdict {
        Verdict::Allow
    }
    fn prepare(&self, _: &ProjectId, _: &Invocation) -> Result<(), String> {
        Ok(())
    }
    fn started(&self, _: &ProjectId, _: &Invocation) -> Result<(), String> {
        Ok(())
    }
    fn ended(&self, _: &ProjectId, _: &Invocation) -> Result<(), String> {
        Ok(())
    }
    fn aborted(&self, _: &ProjectId, _: &Invocation, _: bool) -> Result<(), String> {
        Ok(())
    }
}

async fn run(
    rebound: Option<u64>,
    fail_once: bool,
    refuse_after: usize,
    engine_expected: Option<u64>,
) -> (bool, usize, usize) {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = Engine::new(Store::open(dir.path()).unwrap(), Registry::new());
    if let Some(expected) = engine_expected {
        engine = engine.guarded(Arc::new(Guard {
            expected,
            checks: AtomicUsize::new(0),
            refuse_after: usize::MAX,
        }));
    }
    let project = engine.create_project("model guard", "").unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let guard = Arc::new(Guard {
        expected: 7,
        checks: AtomicUsize::new(0),
        refuse_after,
    });
    let model = Arc::new(Model {
        revision: 7,
        rebound,
        calls: calls.clone(),
        fail_once,
    });
    let router = ModelRouter::single(model)
        .for_conversation("fixture")
        .unwrap();
    let result = Harness::new(router)
        .planner(false)
        .critic(false)
        .guard(guard.clone())
        .ask(
            &engine,
            &AgentProfile::default(),
            &project.id,
            "Reply ready",
            |_| {},
        )
        .await;
    (
        result.is_ok(),
        calls.load(Ordering::SeqCst),
        guard.checks.load(Ordering::SeqCst),
    )
}

#[tokio::test]
async fn an_account_changed_by_conversation_binding_is_refused_before_inference() {
    let (ok, calls, _) = run(Some(8), false, usize::MAX, None).await;
    assert!(!ok);
    assert_eq!(calls, 0);
}

#[tokio::test]
async fn a_connection_is_checked_again_before_a_provider_retry() {
    let (ok, calls, checks) = run(None, true, 1, None).await;
    assert!(!ok);
    assert_eq!(calls, 1);
    assert_eq!(checks, 2);
}

#[tokio::test]
async fn the_frozen_connection_still_completes_work() {
    let (ok, calls, _) = run(None, false, usize::MAX, None).await;
    assert!(ok);
    assert_eq!(calls, 1);
}

#[tokio::test]
async fn intersected_guards_preserve_either_model_refusal() {
    for (expected, refuse_after) in [(9, usize::MAX), (7, 0)] {
        let (ok, calls, _) = run(None, false, refuse_after, Some(expected)).await;
        assert!(!ok);
        assert_eq!(calls, 0);
    }
}
