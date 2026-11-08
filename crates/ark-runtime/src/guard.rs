//! Per-run limits apply before approval shortcuts and before an adapter starts.
use ark_core::{ActionDescriptor, Invocation, ProjectId};
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Refuse(String),
    Allow,
    /// A mutation needs a live approval. Other calls refuse this verdict.
    Ask,
}

pub trait CallGuard: Send + Sync {
    fn check(
        &self,
        action: &str,
        input: &serde_json::Value,
        descriptor: Option<&ActionDescriptor>,
    ) -> Verdict;
    fn prepare(&self, project: &ProjectId, invocation: &Invocation) -> Result<(), String>;
    fn started(&self, project: &ProjectId, invocation: &Invocation) -> Result<(), String>;
    fn ended(&self, project: &ProjectId, invocation: &Invocation) -> Result<(), String>;
    fn aborted(
        &self,
        project: &ProjectId,
        invocation: &Invocation,
        started: bool,
    ) -> Result<(), String>;
}

struct Intersection(Arc<dyn CallGuard>, Arc<dyn CallGuard>);

impl CallGuard for Intersection {
    fn check(
        &self,
        action: &str,
        input: &serde_json::Value,
        descriptor: Option<&ActionDescriptor>,
    ) -> Verdict {
        let first = self.0.check(action, input, descriptor);
        if matches!(first, Verdict::Refuse(_)) {
            return first;
        }
        let second = self.1.check(action, input, descriptor);
        match (&first, &second) {
            (_, Verdict::Refuse(_)) => second,
            (Verdict::Ask, _) | (_, Verdict::Ask) => Verdict::Ask,
            _ => Verdict::Allow,
        }
    }
    fn prepare(&self, project: &ProjectId, invocation: &Invocation) -> Result<(), String> {
        self.0.prepare(project, invocation)?;
        if let Err(error) = self.1.prepare(project, invocation) {
            return match self.0.aborted(project, invocation, false) {
                Ok(()) => Err(error),
                Err(cleanup) => Err(format!(
                    "{error}; could not release the first reservation: {cleanup}"
                )),
            };
        }
        Ok(())
    }
    fn started(&self, project: &ProjectId, invocation: &Invocation) -> Result<(), String> {
        self.0.started(project, invocation)?;
        self.1.started(project, invocation)
    }
    fn aborted(
        &self,
        project: &ProjectId,
        invocation: &Invocation,
        started: bool,
    ) -> Result<(), String> {
        let first = self.0.aborted(project, invocation, started);
        let second = self.1.aborted(project, invocation, started);
        match (first, second) {
            (Err(first), Err(second)) => Err(format!("{first}; {second}")),
            (Err(error), _) | (_, Err(error)) => Err(error),
            _ => Ok(()),
        }
    }
    fn ended(&self, project: &ProjectId, invocation: &Invocation) -> Result<(), String> {
        self.0.ended(project, invocation)?;
        self.1.ended(project, invocation)
    }
}

pub(crate) fn narrow(
    current: Option<Arc<dyn CallGuard>>,
    next: Arc<dyn CallGuard>,
) -> Arc<dyn CallGuard> {
    match current {
        Some(current) if !Arc::ptr_eq(&current, &next) => Arc::new(Intersection(current, next)),
        Some(current) => current,
        None => next,
    }
}

pub(crate) fn safe_meta(action: &str) -> bool {
    matches!(
        action,
        crate::catalog::LIST_ACTIONS
            | crate::catalog::LOAD_ACTION
            | crate::catalog::GET_EVIDENCE
            | crate::catalog::SEARCH_EVIDENCE
    )
}

pub(crate) struct Reservation {
    guard: Arc<dyn CallGuard>,
    project: ProjectId,
    invocation: Invocation,
    armed: bool,
    started: bool,
}
impl Reservation {
    pub(crate) fn new(
        guard: Arc<dyn CallGuard>,
        project: ProjectId,
        invocation: Invocation,
    ) -> Self {
        Self {
            guard,
            project,
            invocation,
            armed: false,
            started: false,
        }
    }
    pub(crate) fn prepare(&mut self) -> Result<(), String> {
        self.guard.prepare(&self.project, &self.invocation)?;
        self.armed = true;
        Ok(())
    }
    pub(crate) fn started(&mut self) -> Result<(), String> {
        self.guard.started(&self.project, &self.invocation)?;
        self.started = true;
        Ok(())
    }
    pub(crate) fn finish(&mut self, invocation: &Invocation) -> Result<(), String> {
        self.invocation = invocation.clone();
        self.guard.ended(&self.project, &self.invocation)?;
        self.armed = false;
        Ok(())
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        if self.armed {
            if let Err(error) = self
                .guard
                .aborted(&self.project, &self.invocation, self.started)
            {
                tracing::error!(invocation = %self.invocation.id, %error, "could not retain the interrupted effect reservation");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AgentProfile, Engine, Harness, ModelRouter};
    use ark_core::{ActionDescriptor, InvocationStatus, RiskTier};
    use ark_model::{Completion, MockProvider, ToolCall};
    use ark_tools::{ActionOutput, Adapter, Registry, RunCtx};
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Change(Arc<AtomicUsize>);
    #[async_trait::async_trait]
    impl Adapter for Change {
        fn name(&self) -> &str {
            "change"
        }
        fn actions(&self) -> Vec<ActionDescriptor> {
            vec![
                ActionDescriptor::new("change.write", "write a value", RiskTier::Mutate)
                    .auto_eligible(),
            ]
        }
        async fn run(
            &self,
            _action: &str,
            _input: serde_json::Value,
            _ctx: &RunCtx<'_>,
        ) -> ark_tools::Result<ActionOutput> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(ActionOutput::json(json!({"written":true})))
        }
    }

    struct Limits {
        verdict: Verdict,
        fail_prepare: bool,
        fail_ended: bool,
        prepared: std::sync::Mutex<Vec<String>>,
        ended: std::sync::Mutex<Vec<String>>,
        aborted: AtomicUsize,
    }
    impl Limits {
        fn new(verdict: Verdict) -> Arc<Self> {
            Arc::new(Self {
                verdict,
                fail_prepare: false,
                fail_ended: false,
                prepared: Default::default(),
                ended: Default::default(),
                aborted: Default::default(),
            })
        }
    }
    impl CallGuard for Limits {
        fn check(
            &self,
            _action: &str,
            _input: &serde_json::Value,
            _descriptor: Option<&ActionDescriptor>,
        ) -> Verdict {
            self.verdict.clone()
        }
        fn prepare(&self, _project: &ProjectId, invocation: &Invocation) -> Result<(), String> {
            self.prepared
                .lock()
                .unwrap()
                .push(invocation.id.to_string());
            if self.fail_prepare {
                Err("could not reserve the effect".into())
            } else {
                Ok(())
            }
        }
        fn started(&self, _project: &ProjectId, _invocation: &Invocation) -> Result<(), String> {
            Ok(())
        }
        fn aborted(
            &self,
            _project: &ProjectId,
            _invocation: &Invocation,
            _started: bool,
        ) -> Result<(), String> {
            self.aborted.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn ended(&self, _project: &ProjectId, invocation: &Invocation) -> Result<(), String> {
            self.ended.lock().unwrap().push(invocation.id.to_string());
            if self.fail_ended {
                Err("could not confirm the effect".into())
            } else {
                Ok(())
            }
        }
    }
    fn engine(dir: &std::path::Path, calls: Arc<AtomicUsize>) -> Engine {
        let mut registry = Registry::new();
        registry.register(Arc::new(Change(calls)));
        let engine = Engine::new(ark_store::Store::open(dir).unwrap(), registry);
        let mut project = ark_core::Project::new("p", "test");
        project.id = ProjectId("p".into());
        engine.store.put_project(&project).unwrap();
        engine
    }

    #[test]
    fn failed_second_guard_releases_only_the_successful_first_reservation() {
        let first = Limits::new(Verdict::Allow);
        let mut second = Limits::new(Verdict::Allow);
        Arc::get_mut(&mut second).unwrap().fail_prepare = true;
        let guard = Intersection(first.clone(), second.clone());
        let invocation = Invocation::new("change.write", RiskTier::Mutate, json!({}));
        assert!(guard.prepare(&ProjectId::new(), &invocation).is_err());
        assert_eq!(first.aborted.load(Ordering::SeqCst), 1);
        assert_eq!(second.aborted.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn public_mutation_shortcuts_cannot_satisfy_a_guard_approval_or_widen_it() {
        let dir = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let engine = engine(dir.path(), calls.clone());
        let ask = Limits::new(Verdict::Ask);
        let guarded = engine.guarded(ask.clone());
        let invocation = guarded
            .run_action(&ProjectId("p".into()), "change.write", json!({}), true)
            .await
            .unwrap();
        assert_eq!(invocation.status, InvocationStatus::PendingApproval);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(ask.prepared.lock().unwrap().is_empty());
        let denied = engine.guarded(Limits::new(Verdict::Refuse("outside the run".into())));
        let narrowed = denied.guarded(Limits::new(Verdict::Allow));
        assert!(narrowed
            .run_action(&ProjectId("p".into()), "change.write", json!({}), true)
            .await
            .is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn harness_auto_and_allow_mutate_do_not_replace_a_run_approval() {
        let dir = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let engine = engine(dir.path(), calls.clone());
        let model = Arc::new(MockProvider::new(
            "test",
            vec![
                Completion {
                    content: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "one".into(),
                        name: "change.write".into(),
                        arguments: json!({}),
                    }],
                    usage: Default::default(),
                },
                Completion {
                    content: "cannot run without approval".into(),
                    tool_calls: vec![],
                    usage: Default::default(),
                },
            ],
        ));
        let profile = AgentProfile::builder()
            .core_actions(["change.write"])
            .build();
        Harness::new(ModelRouter::single(model))
            .planner(false)
            .critic(false)
            .max_steps(2)
            .allow_mutate(true)
            .execution_mode(ark_core::ExecutionMode::Auto)
            .guard(Limits::new(Verdict::Ask))
            .ask(
                &engine,
                &profile,
                &ProjectId("p".into()),
                "write a value",
                |_| {},
            )
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn accounting_errors_stop_effects_or_surface_failed_confirmation_with_the_same_invocation(
    ) {
        let dir = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let engine = engine(dir.path(), calls.clone());
        let failed = Arc::new(Limits {
            verdict: Verdict::Allow,
            fail_prepare: true,
            fail_ended: false,
            prepared: Default::default(),
            ended: Default::default(),
            aborted: Default::default(),
        });
        assert!(engine
            .guarded(failed)
            .run_action(&ProjectId("p".into()), "change.write", json!({}), false)
            .await
            .is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let failed = Arc::new(Limits {
            verdict: Verdict::Allow,
            fail_prepare: false,
            fail_ended: true,
            prepared: Default::default(),
            ended: Default::default(),
            aborted: Default::default(),
        });
        assert!(engine
            .guarded(failed.clone())
            .run_action(&ProjectId("p".into()), "change.write", json!({}), false)
            .await
            .is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            *failed.prepared.lock().unwrap(),
            *failed.ended.lock().unwrap()
        );
    }
    struct Pause(Arc<tokio::sync::Notify>);
    #[async_trait::async_trait]
    impl Adapter for Pause {
        fn name(&self) -> &str {
            "pause"
        }
        fn actions(&self) -> Vec<ActionDescriptor> {
            vec![ActionDescriptor::new(
                "pause.write",
                "wait while changing",
                RiskTier::Mutate,
            )]
        }
        async fn run(
            &self,
            _action: &str,
            _input: serde_json::Value,
            _ctx: &RunCtx<'_>,
        ) -> ark_tools::Result<ActionOutput> {
            self.0.notify_one();
            std::future::pending().await
        }
    }

    #[tokio::test]
    async fn dropping_an_invocation_notifies_the_guard_instead_of_confirming_it() {
        let dir = tempfile::tempdir().unwrap();
        let entered = Arc::new(tokio::sync::Notify::new());
        let mut engine = engine(dir.path(), Arc::default());
        engine.registry.register(Arc::new(Pause(entered.clone())));
        let guard = Limits::new(Verdict::Allow);
        let engine = engine.guarded(guard.clone());
        let task = tokio::spawn(async move {
            engine
                .run_action(&ProjectId("p".into()), "pause.write", json!({}), false)
                .await
        });
        entered.notified().await;
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(guard.prepared.lock().unwrap().len(), 1);
        assert!(guard.ended.lock().unwrap().is_empty());
        assert_eq!(guard.aborted.load(Ordering::SeqCst), 1);
    }
    struct Probe(Arc<AtomicUsize>, RiskTier);
    #[async_trait::async_trait]
    impl Adapter for Probe {
        fn name(&self) -> &str {
            "probe"
        }
        fn actions(&self) -> Vec<ActionDescriptor> {
            vec![ActionDescriptor::new("probe.read", "read a value", self.1)]
        }
        async fn run(
            &self,
            _action: &str,
            _input: serde_json::Value,
            _ctx: &RunCtx<'_>,
        ) -> ark_tools::Result<ActionOutput> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(ActionOutput::json(json!({"read":true})))
        }
    }
    fn one_call(action: &str, input: serde_json::Value) -> Arc<MockProvider> {
        Arc::new(MockProvider::new(
            "test",
            vec![
                Completion {
                    content: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "one".into(),
                        name: action.into(),
                        arguments: input,
                    }],
                    usage: Default::default(),
                },
                Completion {
                    content: "finished".into(),
                    tool_calls: vec![],
                    usage: Default::default(),
                },
            ],
        ))
    }

    #[tokio::test]
    async fn ask_for_read_or_observe_fails_closed_in_engine_and_harness() {
        for risk in [RiskTier::Read, RiskTier::Observe] {
            let dir = tempfile::tempdir().unwrap();
            let calls = Arc::new(AtomicUsize::new(0));
            let mut engine = engine(dir.path(), Arc::default());
            engine
                .registry
                .register(Arc::new(Probe(calls.clone(), risk)));
            let guarded = engine.guarded(Limits::new(Verdict::Ask));
            assert!(guarded
                .run_action(&ProjectId("p".into()), "probe.read", json!({}), true)
                .await
                .is_err());
            let model = one_call("probe.read", json!({}));
            Harness::new(ModelRouter::single(model))
                .planner(false)
                .critic(false)
                .max_steps(2)
                .allow_mutate(true)
                .guard(Limits::new(Verdict::Ask))
                .ask(
                    &engine,
                    &AgentProfile::builder().core_actions(["probe.read"]).build(),
                    &ProjectId("p".into()),
                    "read a value",
                    |_| {},
                )
                .await
                .unwrap();
            assert_eq!(calls.load(Ordering::SeqCst), 0);
        }
    }

    struct Acquire(Arc<AtomicUsize>);
    #[async_trait::async_trait]
    impl crate::AcquisitionProvider for Acquire {
        async fn find(&self, _need: &str) -> String {
            "available".into()
        }
        async fn propose(&self, _need: &str) -> String {
            "connect".into()
        }
        async fn acquire(&self, _need: &str, approved: bool) -> crate::AcquisitionResult {
            if approved {
                self.0.fetch_add(1, Ordering::SeqCst);
                crate::AcquisitionResult::changed("connected")
            } else {
                crate::AcquisitionResult::info("approval needed")
            }
        }
    }

    #[tokio::test]
    async fn protected_meta_calls_cannot_acquire_capabilities_through_allow_mutate() {
        for verdict in [None, Some(Verdict::Ask), Some(Verdict::Allow)] {
            let dir = tempfile::tempdir().unwrap();
            let calls = Arc::new(AtomicUsize::new(0));
            let engine = engine(dir.path(), Arc::default());
            let model = one_call(
                crate::catalog::ACQUIRE_CAPABILITY,
                json!({"need":"test capability"}),
            );
            let profile = AgentProfile::builder()
                .acquisition(Arc::new(Acquire(calls.clone())))
                .build();
            let mut harness = Harness::new(ModelRouter::single(model))
                .planner(false)
                .critic(false)
                .max_steps(2)
                .allow_mutate(true);
            if let Some(verdict) = &verdict {
                harness = harness.guard(Limits::new(verdict.clone()));
            }
            harness
                .ask(
                    &engine,
                    &profile,
                    &ProjectId("p".into()),
                    "connect the capability",
                    |_| {},
                )
                .await
                .unwrap();
            assert_eq!(calls.load(Ordering::SeqCst), usize::from(verdict.is_none()));
        }
    }
    #[tokio::test]
    async fn guarded_meta_writes_cannot_change_notes_or_working_state() {
        for (action, arguments) in [
            (crate::catalog::RECORD_NOTE, json!({"text":"not allowed"})),
            (
                crate::catalog::RECORD_STATE,
                json!({"kind":"note","text":"not allowed"}),
            ),
            (
                crate::catalog::RECORD_STATE,
                json!({"kind":"question","text":"not allowed"}),
            ),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let engine = engine(dir.path(), Arc::default());
            let model = one_call(action, arguments);
            Harness::new(ModelRouter::single(model))
                .planner(false)
                .critic(false)
                .max_steps(2)
                .allow_mutate(true)
                .guard(Limits::new(Verdict::Allow))
                .ask(
                    &engine,
                    &AgentProfile::default(),
                    &ProjectId("p".into()),
                    "record a note",
                    |_| {},
                )
                .await
                .unwrap();
            assert!(engine
                .store
                .list_notes(&ProjectId("p".into()))
                .unwrap()
                .iter()
                .all(|note| note.body != "not allowed"));
            assert!(!serde_json::to_string(
                &engine
                    .store
                    .get_research_state(&ProjectId("p".into()), "")
                    .unwrap()
            )
            .unwrap()
            .contains("not allowed"));
        }
    }
}
