use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, Mutex,
};

use ark_model::{Completion, MockProvider, ToolCall, Usage};
use ark_runtime::connection_requests::*;
use ark_sdk::prelude::*;
use ark_tools::ClaimedConnectionBinding;
use serde_json::{json, Value};

#[derive(Default)]
struct Tools {
    writes: AtomicUsize,
    connects: AtomicUsize,
    bindings: Mutex<Vec<ClaimedConnectionBinding>>,
    metadata_unavailable: AtomicBool,
    metadata_attempts: AtomicUsize,
    invalid_metadata: AtomicBool,
}

#[async_trait::async_trait]
impl Adapter for Tools {
    fn name(&self) -> &str {
        "test"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![
            ActionDescriptor::new("test.write", "make one change", RiskTier::Mutate),
            ActionDescriptor::new("test.connect", "request an app", RiskTier::Observe)
                .contextual_acquisition(),
        ]
    }
    async fn run(
        &self,
        action: &str,
        _input: Value,
        ctx: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        if action == "test.write" {
            assert!(ctx.connection_binding.is_none());
            self.writes.fetch_add(1, Ordering::SeqCst);
            Ok(ActionOutput::json(json!({"changed":true})))
        } else {
            self.metadata_attempts.fetch_add(1, Ordering::SeqCst);
            if self.metadata_unavailable.load(Ordering::SeqCst) {
                return Err(AdapterError::Tool("fixture metadata unavailable".into()));
            }
            let binding = ctx
                .connection_binding
                .as_ref()
                .expect("exact trusted binding")
                .clone();
            assert_eq!(
                ctx.invocation.as_deref(),
                Some(binding.invocation_id.as_str())
            );
            tokio::task::yield_now().await;
            self.bindings.lock().unwrap().push(binding.clone());
            self.connects.fetch_add(1, Ordering::SeqCst);
            let mut output = ActionOutput::json(json!({"bindingId":binding.binding_id}));
            if self.invalid_metadata.load(Ordering::SeqCst) {
                output.command = Some(vec!["invalid-metadata-result".into()]);
            }
            Ok(output)
        }
    }
}

struct Source;
#[async_trait::async_trait]
impl AcquisitionProvider for Source {
    fn knows(&self, need: &str) -> bool {
        need == "sample"
    }
    async fn find(&self, _: &str) -> String {
        String::new()
    }
    async fn propose(&self, _: &str) -> String {
        String::new()
    }
    async fn acquire(&self, _: &str, _: bool) -> AcquisitionResult {
        AcquisitionResult::info("not used")
    }
    async fn contextual_acquisition(
        &self,
        action: &ActionDescriptor,
        input: &Value,
    ) -> ConnectionResult<Option<ContextualAcquisition>> {
        Ok(
            (action.name == "test.connect").then(|| ContextualAcquisition {
                offer_id: input["offer"].as_str().unwrap().into(),
                display: "Connect Sample".into(),
            }),
        )
    }
}

#[derive(Default)]
struct Broker {
    ready: AtomicBool,
    mismatch: AtomicBool,
    lose_claim: AtomicBool,
    cancelled: AtomicBool,
    claims: AtomicUsize,
    resolves: AtomicUsize,
    rows: Mutex<HashMap<String, (ConnectionScopeRequest, Option<ClaimedConnectionBinding>)>>,
}

#[async_trait::async_trait]
impl ConnectionRequestBroker for Broker {
    fn route_id(&self) -> &str {
        "route-one"
    }
    async fn prepare(
        &self,
        request: ConnectionScopeRequest,
    ) -> ConnectionResult<PreparedConnectionScope> {
        let scope = format!("scope-{}", request.request_id);
        let mut rows = self.rows.lock().unwrap();
        if let Some((prior, _)) = rows.get(&scope) {
            assert_eq!(prior.invocation_id, request.invocation_id);
            assert_eq!(prior.offer_id, request.offer_id);
        } else {
            rows.insert(scope.clone(), (request, None));
        }
        Ok(PreparedConnectionScope {
            scope_ref: scope,
            broker_revision: 9_007_199_254_740_993,
            expires_at: "2030-01-01T00:00:00Z".into(),
        })
    }
    async fn resolve(
        &self,
        request: ResolveConnectionScope,
    ) -> ConnectionResult<ResolvedConnectionScope> {
        self.resolves.fetch_add(1, Ordering::SeqCst);
        let rows = self.rows.lock().unwrap();
        let (saved, claim) = rows.get(&request.scope_ref).unwrap();
        assert_eq!(request.invocation_id, saved.invocation_id);
        Ok(ResolvedConnectionScope {
            scope_ref: request.scope_ref,
            request_id: request.request_id,
            invocation_id: request.invocation_id,
            route_id: if self.mismatch.load(Ordering::SeqCst) {
                "another-route".into()
            } else {
                request.route_id
            },
            offer_id: saved.offer_id.clone(),
            fingerprint: "reviewed-schema-one".into(),
            broker_revision: 9_007_199_254_740_993 + u64::from(claim.is_some()),
            state: if self.cancelled.load(Ordering::SeqCst) {
                ConnectionScopeState::Cancelled
            } else if claim.is_some() {
                ConnectionScopeState::Claimed
            } else if self.ready.load(Ordering::SeqCst) {
                ConnectionScopeState::Ready
            } else {
                ConnectionScopeState::Pending
            },
            claim_receipt: claim.clone(),
        })
    }
    async fn claim(
        &self,
        request: ClaimConnectionScope,
    ) -> ConnectionResult<ClaimedConnectionBinding> {
        self.claims.fetch_add(1, Ordering::SeqCst);
        assert_eq!(request.expected_broker_revision, 9_007_199_254_740_993);
        let mut rows = self.rows.lock().unwrap();
        let (saved, claim) = rows.get_mut(&request.scope_ref).unwrap();
        assert_eq!(saved.invocation_id, request.invocation_id);
        let binding = claim
            .get_or_insert_with(|| ClaimedConnectionBinding {
                receipt_id: "receipt-one".into(),
                scope_ref: request.scope_ref,
                request_id: request.request_id,
                invocation_id: request.invocation_id,
                route_id: "route-one".into(),
                offer_id: saved.offer_id.clone(),
                binding_id: "chosen-account".into(),
                fingerprint: "reviewed-schema-one".into(),
                broker_revision: 9_007_199_254_740_994,
            })
            .clone();
        if self.lose_claim.swap(false, Ordering::SeqCst) {
            return Err(ConnectionRequestError::Unavailable("response lost".into()));
        }
        Ok(binding)
    }
    async fn cancel(
        &self,
        request: ClaimConnectionScope,
    ) -> ConnectionResult<ResolvedConnectionScope> {
        self.cancelled.store(true, Ordering::SeqCst);
        self.resolve(ResolveConnectionScope {
            scope_ref: request.scope_ref,
            request_id: request.request_id,
            invocation_id: request.invocation_id,
            route_id: self.route_id().into(),
        })
        .await
    }
}

fn call(name: &str, input: Value) -> ToolCall {
    ToolCall {
        id: name.into(),
        name: name.into(),
        arguments: input,
    }
}
fn step(calls: Vec<ToolCall>) -> Completion {
    Completion {
        content: String::new(),
        tool_calls: calls,
        usage: Usage::default(),
    }
}
fn answer() -> Completion {
    Completion {
        content: "Finished.".into(),
        tool_calls: vec![],
        usage: Usage::default(),
    }
}
fn app(
    path: &std::path::Path,
    tools: Arc<Tools>,
    broker: Arc<Broker>,
    script: Vec<Completion>,
) -> Ark {
    Ark::builder()
        .store_root(path)
        .tool(tools)
        .connection_requests(broker)
        .acquisition_source(Arc::new(Source))
        .profile(
            AgentProfile::builder()
                .core_actions(["test.write", "test.connect"])
                .build(),
        )
        .provider(Arc::new(MockProvider::new("fixture", script)))
        .allow_mutate(true)
        .planner(false)
        .critic(false)
        .build()
        .unwrap()
}

async fn paused(ark: &Ark) -> (Session, String) {
    let project = ark.find_or_create_project("p", "").unwrap();
    let mut session = ark.start_session(&project.id).unwrap();
    let outcome = ark
        .ask_session(
            &mut session,
            "Make the requested change with Sample",
            TurnOptions::default(),
            |_| {},
        )
        .await
        .unwrap();
    let pending = outcome.awaiting_connection.expect("typed pause");
    assert_eq!(session.turn_count(), 1);
    assert!(session.transcript(ark.engine(), None).unwrap()[0].incomplete);
    (session, pending.request_id)
}

async fn started(ark: &Ark, broker: &Broker) -> (Session, String) {
    let (mut session, id) = paused(ark).await;
    broker.ready.store(true, Ordering::SeqCst);
    let request = ark.engine().refresh_connection_request(&id).await.unwrap();
    let run = ark
        .engine()
        .admit_connection(&id, request.local_revision)
        .unwrap();
    let turn = ark.engine().start_connection(&run).unwrap();
    session.resume_waiting(ark.engine(), turn).unwrap();
    (session, id)
}

fn partial_outcome() -> HarnessOutcome {
    let mut metrics = ark_runtime::metrics::Metrics::default();
    metrics.prompt_tokens = 17;
    metrics.completion_tokens = 11;
    HarnessOutcome {
        awaiting_connection: None,
        answer: "The part already answered".into(),
        cited: vec![ark_core::InvocationId::new()],
        steps: 1,
        trivial: false,
        metrics,
        state: Default::default(),
        cancelled: false,
        interrupted: false,
        steered: vec![],
        ran_out: false,
    }
}

#[tokio::test]
async fn stopped_resumed_finalization_keeps_answer_citations_metrics_and_closed_fence() {
    let dir = tempfile::tempdir().unwrap();
    let tools = Arc::new(Tools::default());
    let broker = Arc::new(Broker::default());
    let ark = app(
        dir.path(),
        tools.clone(),
        broker.clone(),
        vec![step(vec![call("test.connect", json!({"offer":"sample"}))])],
    );
    let (mut session, id) = started(&ark, &broker).await;
    let original = session.transcript(ark.engine(), None).unwrap().remove(0);
    assert!(ark.control_session(session.id().as_str(), true).unwrap());
    let stopped = ark.engine().connection_request(&id).unwrap();
    assert!(
        matches!(&stopped.phase, ConnectionRequestPhase::Closed { reason } if reason == "cancelled")
    );
    let held = session.transcript(ark.engine(), None).unwrap().remove(0);
    assert_eq!(held.connection, original.connection);
    assert!(held.pid.is_none());
    let outcome = partial_outcome();
    session.record_turn(ark.engine(), "", &outcome).unwrap();
    let turns = session.transcript(ark.engine(), None).unwrap();
    assert_eq!(turns.len(), 1);
    let final_turn = &turns[0];
    assert_eq!(final_turn.input, original.input);
    assert_eq!(final_turn.answer, outcome.answer);
    assert!(final_turn.cancelled);
    assert!(final_turn.connection.is_none());
    assert!(final_turn
        .cited
        .contains(&outcome.cited[0].short().to_string()));
    assert_eq!(
        final_turn.metrics.prompt_tokens,
        original.metrics.prompt_tokens + 17
    );
    assert_eq!(
        final_turn.metrics.completion_tokens,
        original.metrics.completion_tokens + 11
    );
    let final_request = ark.engine().connection_request(&id).unwrap();
    assert_eq!(final_request.phase, stopped.phase);
    assert_eq!(final_request.cancel_pending, stopped.cancel_pending);
    assert!(ark
        .engine()
        .admit_connection(&id, final_request.local_revision)
        .is_err());
    assert_eq!(tools.connects.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn stop_during_new_prerequisite_suppresses_returned_pause_and_durable_ghosts() {
    let dir = tempfile::tempdir().unwrap();
    let tools = Arc::new(Tools::default());
    let broker = Arc::new(Broker::default());
    let ark = app(
        dir.path(),
        tools.clone(),
        broker.clone(),
        vec![
            step(vec![call("test.connect", json!({"offer":"sample"}))]),
            step(vec![call("test.connect", json!({"offer":"another"}))]),
        ],
    );
    let (mut session, id) = paused(&ark).await;
    broker.ready.store(true, Ordering::SeqCst);
    let request = ark.engine().refresh_connection_request(&id).await.unwrap();
    let run = ark
        .engine()
        .admit_connection(&id, request.local_revision)
        .unwrap();
    let session_id = session.id().to_string();
    let mut stopped = false;
    let outcome = ark
        .resume_connection(&mut session, run, TurnOptions::default(), |event| {
            if matches!(event, HarnessEvent::AwaitingConnection { .. }) {
                ark.control_session(&session_id, true).unwrap();
                stopped = true;
            }
        })
        .await
        .unwrap();
    assert!(stopped);
    assert!(outcome.cancelled);
    assert!(outcome.awaiting_connection.is_none());
    let requests = ark.engine().connection_requests(&session_id).unwrap();
    assert_eq!(requests.len(), 1);
    assert!(
        matches!(&requests[0].phase, ConnectionRequestPhase::Closed { reason } if reason == "cancelled")
    );
    assert_eq!(
        ark.engine()
            .store
            .project_stats(session.project())
            .unwrap()
            .invocations,
        1
    );
    assert_eq!(tools.connects.load(Ordering::SeqCst), 1);
    assert_eq!(session.turn_count(), 1);
    let turn = session.transcript(ark.engine(), None).unwrap().remove(0);
    assert!(turn.connection.is_none());
    assert!(!turn.incomplete);
    assert!(!turn.ran_out);
}

#[tokio::test]
async fn stopped_resumed_late_finalizer_cannot_overwrite_replacement_input() {
    let dir = tempfile::tempdir().unwrap();
    let tools = Arc::new(Tools::default());
    let broker = Arc::new(Broker::default());
    let ark = app(
        dir.path(),
        tools,
        broker.clone(),
        vec![step(vec![call("test.connect", json!({"offer":"sample"}))])],
    );
    let (mut old, _) = started(&ark, &broker).await;
    ark.control_session(old.id().as_str(), true).unwrap();
    let mut replacement = ark.open_session(old.id().as_str()).unwrap().unwrap();
    replacement
        .admit_input(ark.engine(), "Use this newer instruction")
        .unwrap();
    let header = ark.engine().store.get_session(old.id().as_str()).unwrap();
    let turns = ark
        .engine()
        .store
        .list_session_turns(old.id().as_str(), None)
        .unwrap();
    assert!(old
        .record_turn(ark.engine(), "", &partial_outcome())
        .is_err());
    assert_eq!(
        ark.engine().store.get_session(old.id().as_str()).unwrap(),
        header
    );
    assert_eq!(
        ark.engine()
            .store
            .list_session_turns(old.id().as_str(), None)
            .unwrap(),
        turns
    );
    assert_eq!(turns.last().unwrap()["input"], "Use this newer instruction");
}

#[tokio::test]
async fn stopped_dead_writer_recovery_clears_only_owned_marker_and_unblocks_rename() {
    let dir = tempfile::tempdir().unwrap();
    let tools = Arc::new(Tools::default());
    let broker = Arc::new(Broker::default());
    let ark = app(
        dir.path(),
        tools,
        broker.clone(),
        vec![step(vec![call("test.connect", json!({"offer":"sample"}))])],
    );
    let (session, id) = started(&ark, &broker).await;
    ark.control_session(session.id().as_str(), true).unwrap();
    let mut row = ark
        .engine()
        .store
        .list_session_turns(session.id().as_str(), None)
        .unwrap()
        .remove(0);
    row["connection"]["pid_start"] = json!(u64::MAX);
    ark.engine()
        .store
        .put_session_turn(session.id().as_str(), 0, &row)
        .unwrap();
    let header = ark
        .engine()
        .store
        .get_session(session.id().as_str())
        .unwrap();
    let request = ark.engine().store.get_record(RECORD_KIND, &id).unwrap();
    assert_eq!(
        ark.engine()
            .recover_connection_requests(Some(session.id().as_str()))
            .unwrap(),
        1
    );
    assert_eq!(
        ark.engine()
            .store
            .get_session(session.id().as_str())
            .unwrap(),
        header
    );
    assert_eq!(
        ark.engine().store.get_record(RECORD_KIND, &id).unwrap(),
        request
    );
    row.as_object_mut().unwrap().remove("connection");
    assert_eq!(
        ark.engine()
            .store
            .list_session_turns(session.id().as_str(), None)
            .unwrap(),
        [row]
    );
    let mut reopened = ark.open_session(session.id().as_str()).unwrap().unwrap();
    reopened.set_title(ark.engine(), "Stopped chat").unwrap();
    assert_eq!(reopened.turn_count(), 1);
}

#[tokio::test]
async fn waiting_restart_claim_and_resume_keep_one_message_and_one_prior_effect() {
    let dir = tempfile::tempdir().unwrap();
    let tools = Arc::new(Tools::default());
    let broker = Arc::new(Broker::default());
    let first = app(
        dir.path(),
        tools.clone(),
        broker.clone(),
        vec![step(vec![
            call("test.write", json!({})),
            call("test.connect", json!({"offer":"sample"})),
            call("test.write", json!({"unexpected":true})),
        ])],
    );
    let (session, id) = paused(&first).await;
    let session_id = session.id().to_string();
    let project = session.project().clone();
    let pending = first
        .engine()
        .refresh_connection_request(&id)
        .await
        .unwrap();
    assert_eq!(pending.id, id);
    assert_eq!(tools.writes.load(Ordering::SeqCst), 1);
    assert_eq!(tools.connects.load(Ordering::SeqCst), 0);
    drop(session);
    drop(first);
    broker.ready.store(true, Ordering::SeqCst);
    let next = app(
        dir.path(),
        tools.clone(),
        broker,
        vec![step(vec![call("test.write", json!({}))]), answer()],
    );
    let mut session = next.open_session(&session_id).unwrap().unwrap();
    let pending = next.engine().refresh_connection_request(&id).await.unwrap();
    let run = next
        .engine()
        .admit_connection(&id, pending.local_revision)
        .unwrap();
    let outcome = next
        .resume_connection(&mut session, run, TurnOptions::default(), |_| {})
        .await
        .unwrap();
    assert_eq!(outcome.answer, "Finished.");
    assert_eq!(tools.writes.load(Ordering::SeqCst), 1);
    assert_eq!(tools.connects.load(Ordering::SeqCst), 1);
    assert_eq!(session.turn_count(), 1);
    let transcript = session.transcript(next.engine(), None).unwrap();
    assert_eq!(transcript.len(), 1);
    assert_eq!(transcript[0].input, "Make the requested change with Sample");
    assert!(!transcript[0].incomplete);
    let calls = next.engine().store.list_invocations(&project).unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.action == "test.connect")
            .count(),
        1
    );
}

#[tokio::test]
async fn lost_claim_response_reconciles_same_receipt_and_mismatch_never_dispatches() {
    let dir = tempfile::tempdir().unwrap();
    let tools = Arc::new(Tools::default());
    let broker = Arc::new(Broker::default());
    let ark = app(
        dir.path(),
        tools.clone(),
        broker.clone(),
        vec![step(vec![call("test.connect", json!({"offer":"sample"}))])],
    );
    let (_, id) = paused(&ark).await;
    broker.ready.store(true, Ordering::SeqCst);
    broker.mismatch.store(true, Ordering::SeqCst);
    assert!(ark.engine().refresh_connection_request(&id).await.is_err());
    assert_eq!(tools.connects.load(Ordering::SeqCst), 0);
    broker.mismatch.store(false, Ordering::SeqCst);
    broker.lose_claim.store(true, Ordering::SeqCst);
    assert!(ark.engine().refresh_connection_request(&id).await.is_err());
    assert_eq!(tools.connects.load(Ordering::SeqCst), 0);
    let request = ark.engine().refresh_connection_request(&id).await.unwrap();
    assert!(matches!(
        request.phase,
        ConnectionRequestPhase::Acquired { .. }
    ));
    assert_eq!(tools.connects.load(Ordering::SeqCst), 1);
    assert_eq!(broker.claims.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn concurrent_refresh_completes_the_metadata_adapter_once() {
    let dir = tempfile::tempdir().unwrap();
    let tools = Arc::new(Tools::default());
    let broker = Arc::new(Broker::default());
    let ark = app(
        dir.path(),
        tools.clone(),
        broker.clone(),
        vec![step(vec![call("test.connect", json!({"offer":"sample"}))])],
    );
    let (session, id) = paused(&ark).await;
    broker.ready.store(true, Ordering::SeqCst);
    let sibling = ark.clone();
    let (first, second) = tokio::join!(
        ark.engine().refresh_connection_request(&id),
        sibling.engine().refresh_connection_request(&id)
    );
    assert!(matches!(
        first.unwrap().phase,
        ConnectionRequestPhase::Acquired { .. }
    ));
    assert!(matches!(
        second.unwrap().phase,
        ConnectionRequestPhase::Acquired { .. }
    ));
    assert_eq!(tools.connects.load(Ordering::SeqCst), 1);
    assert_eq!(broker.claims.load(Ordering::SeqCst), 1);
    assert_eq!(
        ark.engine()
            .store
            .list_session_turns(session.id().as_str(), None)
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn invalid_metadata_result_is_terminal_not_a_transient_retry() {
    let dir = tempfile::tempdir().unwrap();
    let tools = Arc::new(Tools::default());
    tools.invalid_metadata.store(true, Ordering::SeqCst);
    let broker = Arc::new(Broker::default());
    let ark = app(
        dir.path(),
        tools.clone(),
        broker.clone(),
        vec![step(vec![call("test.connect", json!({"offer":"sample"}))])],
    );
    let (_, id) = paused(&ark).await;
    broker.ready.store(true, Ordering::SeqCst);
    assert!(matches!(
        ark.engine().refresh_connection_request(&id).await,
        Err(ark_runtime::AgentError::Connection(
            ConnectionRequestError::Refused(_)
        ))
    ));
    let request = ark.engine().refresh_connection_request(&id).await.unwrap();
    assert!(
        matches!(request.phase, ConnectionRequestPhase::Closed { reason }
        if reason == "invalid_acquisition_result")
    );
    assert_eq!(tools.metadata_attempts.load(Ordering::SeqCst), 1);
    let invocation = ark
        .engine()
        .store
        .get_invocation(&request.project, &request.invocation)
        .unwrap()
        .unwrap();
    assert_eq!(invocation.status, ark_core::InvocationStatus::Failed);
}

#[tokio::test]
async fn metadata_retry_survives_restart_and_saturates_without_reclaiming_or_replaying() {
    let dir = tempfile::tempdir().unwrap();
    let tools = Arc::new(Tools::default());
    let broker = Arc::new(Broker::default());
    let ark = app(
        dir.path(),
        tools.clone(),
        broker.clone(),
        vec![step(vec![call("test.connect", json!({"offer":"sample"}))])],
    );
    let (session, id) = paused(&ark).await;
    let session_id = session.id().to_string();
    broker.ready.store(true, Ordering::SeqCst);
    tools.metadata_unavailable.store(true, Ordering::SeqCst);
    assert!(matches!(
        ark.engine().refresh_connection_request(&id).await,
        Err(ark_runtime::AgentError::Connection(
            ConnectionRequestError::MetadataUnavailable
        ))
    ));
    let original = ark.engine().connection_request(&id).unwrap();
    assert_eq!(original.metadata_retry.as_ref().unwrap().attempts, 1);
    let mut stored = serde_json::to_value(&original).unwrap();
    let before = stored.clone();
    stored["metadata_retry"]["next_at"] = json!("2030-01-01T00:00:00Z");
    ark.engine()
        .store
        .compare_record(RECORD_KIND, &id, &before, &stored)
        .unwrap();
    let resolves = broker.resolves.load(Ordering::SeqCst);
    drop(session);
    drop(ark);
    let next = app(dir.path(), tools.clone(), broker.clone(), vec![answer()]);
    assert!(matches!(
        next.engine().refresh_connection_request(&id).await,
        Err(ark_runtime::AgentError::Connection(
            ConnectionRequestError::MetadataUnavailable
        ))
    ));
    assert_eq!(broker.resolves.load(Ordering::SeqCst), resolves);
    assert_eq!(tools.metadata_attempts.load(Ordering::SeqCst), 1);
    let before = next
        .engine()
        .store
        .get_record(RECORD_KIND, &id)
        .unwrap()
        .unwrap();
    let mut expired = before.clone();
    expired["metadata_retry"]["next_at"] = json!("2000-01-01T00:00:00Z");
    expired["metadata_retry"]["attempts"] = json!(u32::MAX);
    next.engine()
        .store
        .compare_record(RECORD_KIND, &id, &before, &expired)
        .unwrap();
    assert!(matches!(
        next.engine().refresh_connection_request(&id).await,
        Err(ark_runtime::AgentError::Connection(
            ConnectionRequestError::MetadataUnavailable
        ))
    ));
    let saturated = next.engine().connection_request(&id).unwrap();
    let retry = saturated.metadata_retry.as_ref().unwrap();
    assert_eq!(retry.attempts, u32::MAX);
    let seconds = retry.next_at.unix_timestamp()
        - std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
    assert!((29..=30).contains(&seconds));
    assert_eq!(saturated.invocation, original.invocation);
    assert_eq!(saturated.phase, original.phase);
    let before = serde_json::to_value(&saturated).unwrap();
    let mut due = before.clone();
    due["metadata_retry"]["next_at"] = json!("2000-01-01T00:00:00Z");
    next.engine()
        .store
        .compare_record(RECORD_KIND, &id, &before, &due)
        .unwrap();
    tools.metadata_unavailable.store(false, Ordering::SeqCst);
    let acquired = next.engine().refresh_connection_request(&id).await.unwrap();
    assert!(acquired.metadata_retry.is_none());
    assert_eq!(acquired.invocation, original.invocation);
    assert_eq!(broker.claims.load(Ordering::SeqCst), 1);
    let run = next
        .engine()
        .admit_connection(&id, acquired.local_revision)
        .unwrap();
    let mut session = next.open_session(&session_id).unwrap().unwrap();
    next.resume_connection(&mut session, run, TurnOptions::default(), |_| {})
        .await
        .unwrap();
    assert_eq!(session.turn_count(), 1);
    assert_eq!(session.transcript(next.engine(), None).unwrap().len(), 1);
    assert_eq!(tools.connects.load(Ordering::SeqCst), 1);
    assert_eq!(tools.metadata_attempts.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn admitted_input_dead_writer_recovers_once_without_dispatch_or_counter_rollback() {
    let dir = tempfile::tempdir().unwrap();
    let ark = app(dir.path(), Arc::default(), Arc::default(), vec![]);
    let project = ark.find_or_create_project("p", "").unwrap();
    let mut session = ark.start_session(&project.id).unwrap();
    let id = session.id().to_string();
    let input = "Keep the admitted request after process loss";
    let receipt = json!({"receipt":{"status":"accepted"},"effect":{"turnIndex":0}});
    let highwater = json!({"counter":"7"});
    session
        .admit_input_records(
            ark.engine(),
            input,
            &[
                RecordChange {
                    kind: "client_mutation",
                    id: "fixture:epoch:7",
                    scope: "fixture",
                    expected: None,
                    value: &receipt,
                },
                RecordChange {
                    kind: "client_mutation_highwater",
                    id: "fixture:epoch",
                    scope: "fixture",
                    expected: None,
                    value: &highwater,
                },
            ],
        )
        .unwrap();
    let mut turn = ark.engine().store.list_session_turns(&id, None).unwrap()[0].clone();
    turn["pid"] = json!(std::process::id());
    turn["pid_start"] = json!(u64::MAX);
    ark.engine().store.put_session_turn(&id, 0, &turn).unwrap();
    drop(session);
    drop(ark);
    let next = app(dir.path(), Arc::default(), Arc::default(), vec![]);
    let mut session = next.open_session(&id).unwrap().unwrap();
    assert_eq!(session.turn_count(), 1);
    let turns = session.transcript(next.engine(), None).unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].input, input);
    assert!(turns[0].incomplete);
    assert!(turns[0].pid.is_none());
    assert_eq!(
        turns[0].error.as_deref(),
        Some(ark_runtime::session::INTERRUPTED)
    );
    assert!(next
        .engine()
        .store
        .list_invocations(&project.id)
        .unwrap()
        .is_empty());
    assert_eq!(
        next.engine()
            .store
            .get_record("client_mutation", "fixture:epoch:7")
            .unwrap(),
        Some(receipt)
    );
    assert_eq!(
        next.engine()
            .store
            .get_record("client_mutation_highwater", "fixture:epoch")
            .unwrap(),
        Some(highwater)
    );
    session
        .begin_turn(next.engine(), "A new instruction")
        .unwrap();
    assert_eq!(
        next.engine()
            .store
            .list_session_turns(&id, None)
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn concurrent_sdk_rename_is_busy_and_keeps_the_running_answer() {
    let dir = tempfile::tempdir().unwrap();
    let ark = app(dir.path(), Arc::default(), Arc::default(), vec![answer()]);
    let project = ark.find_or_create_project("p", "").unwrap();
    let mut session = ark.start_session(&project.id).unwrap();
    session.set_title(ark.engine(), "Original title").unwrap();
    let id = session.id().to_string();
    let mut renamed = false;
    let outcome = ark
        .ask_session(
            &mut session,
            "Answer this kept request",
            TurnOptions::default(),
            |_| {
                if !renamed {
                    assert!(matches!(
                        Session::rename(ark.engine(), &id, "Renamed during answer"),
                        Err(ark_runtime::AgentError::Connection(
                            ConnectionRequestError::Busy
                        ))
                    ));
                    renamed = true;
                }
            },
        )
        .await;
    assert!(renamed);
    let outcome = outcome.expect("concurrent rename must preserve the running answer");
    assert_eq!(outcome.answer, "Finished.");
    let fresh = ark.open_session(&id).unwrap().unwrap();
    assert_eq!(fresh.record().title, "Original title");
    let turns = fresh.transcript(ark.engine(), None).unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].answer, "Finished.");
    assert!(!turns[0].incomplete);
}

#[tokio::test]
async fn waiting_rename_preserves_request_but_admitted_and_started_rename_are_busy() {
    let dir = tempfile::tempdir().unwrap();
    let broker = Arc::new(Broker::default());
    let ark = app(
        dir.path(),
        Arc::default(),
        broker.clone(),
        vec![
            step(vec![call("test.connect", json!({"offer":"sample"}))]),
            answer(),
        ],
    );
    let (mut session, id) = paused(&ark).await;
    let before = serde_json::to_value(ark.engine().connection_request(&id).unwrap()).unwrap();
    session.set_title(ark.engine(), "Waiting title").unwrap();
    assert_eq!(
        serde_json::to_value(ark.engine().connection_request(&id).unwrap()).unwrap(),
        before
    );
    broker.ready.store(true, Ordering::SeqCst);
    let acquired = ark.engine().refresh_connection_request(&id).await.unwrap();
    let run = ark
        .engine()
        .admit_connection(&id, acquired.local_revision)
        .unwrap();
    let session_id = session.id().to_string();
    assert!(matches!(
        Session::rename(ark.engine(), &session_id, "Admitted title"),
        Err(ark_runtime::AgentError::Connection(
            ConnectionRequestError::Busy
        ))
    ));
    let mut checked_started = false;
    let outcome = ark
        .resume_connection(&mut session, run, TurnOptions::default(), |_| {
            if !checked_started {
                assert!(matches!(
                    Session::rename(ark.engine(), &session_id, "Started title"),
                    Err(ark_runtime::AgentError::Connection(
                        ConnectionRequestError::Busy
                    ))
                ));
                checked_started = true;
            }
        })
        .await
        .unwrap();
    assert!(checked_started);
    assert_eq!(outcome.answer, "Finished.");
    assert_eq!(session.record().title, "Waiting title");
    let turns = session.transcript(ark.engine(), None).unwrap();
    assert_eq!(turns.len(), 1);
    assert!(!turns[0].incomplete);
    session.set_title(ark.engine(), "Finished title").unwrap();
    assert_eq!(session.record().title, "Finished title");
}

#[tokio::test]
async fn stale_session_rename_and_later_save_cannot_overwrite_a_completed_turn() {
    let dir = tempfile::tempdir().unwrap();
    let ark = app(dir.path(), Arc::default(), Arc::default(), vec![answer()]);
    let project = ark.find_or_create_project("p", "").unwrap();
    let mut current = ark.start_session(&project.id).unwrap();
    current.set_title(ark.engine(), "Original title").unwrap();
    let id = current.id().to_string();
    let mut stale = ark.open_session(&id).unwrap().unwrap();
    ark.ask_session(
        &mut current,
        "Keep the completed answer",
        TurnOptions::default(),
        |_| {},
    )
    .await
    .unwrap();
    let header = ark.engine().store.session_version(&id).unwrap();
    let turns = ark.engine().store.list_session_turns(&id, None).unwrap();
    assert!(stale.set_title(ark.engine(), "Stale title").is_err());
    assert_eq!(stale.record().title, "Original title");
    assert!(stale.set_model(ark.engine(), "stale-model".into()).is_err());
    assert_eq!(ark.engine().store.session_version(&id).unwrap(), header);
    assert_eq!(
        ark.engine().store.list_session_turns(&id, None).unwrap(),
        turns
    );
    assert_eq!(turns[0]["answer"], "Finished.");
}

#[tokio::test]
async fn continuation_setup_failure_interrupts_live_writer_and_releases_chat() {
    let dir = tempfile::tempdir().unwrap();
    let tools = Arc::new(Tools::default());
    let broker = Arc::new(Broker::default());
    let ark = app(
        dir.path(),
        tools.clone(),
        broker.clone(),
        vec![step(vec![call("test.connect", json!({"offer":"sample"}))])],
    );
    let (mut session, id) = paused(&ark).await;
    broker.ready.store(true, Ordering::SeqCst);
    let pending = ark.engine().refresh_connection_request(&id).await.unwrap();
    let run = ark
        .engine()
        .admit_connection(&id, pending.local_revision)
        .unwrap();
    let version = ark
        .engine()
        .store
        .session_version(session.id().as_str())
        .unwrap()
        .unwrap();
    let mut header: Value = serde_json::from_str(&version).unwrap();
    header["dropped_turns"] = json!(1);
    ark.engine()
        .store
        .put_session_if(session.id().as_str(), &version, &header)
        .unwrap();
    assert!(ark
        .resume_connection(&mut session, run, TurnOptions::default(), |_| {})
        .await
        .is_err());
    assert!(matches!(
        ark.engine().connection_request(&id).unwrap().phase,
        ConnectionRequestPhase::Interrupted { .. }
    ));
    let turns = session.transcript(ark.engine(), None).unwrap();
    assert_eq!(turns.len(), 1);
    assert!(turns[0].pid.is_none());
    assert_eq!(turns[0].input, "Make the requested change with Sample");
    assert_eq!(tools.connects.load(Ordering::SeqCst), 1);
    session
        .begin_turn(ark.engine(), "Use the new instruction after setup failed")
        .unwrap();
    assert_eq!(
        ark.engine()
            .store
            .list_session_turns(session.id().as_str(), None)
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn losing_resume_cannot_interrupt_the_started_winner() {
    let dir = tempfile::tempdir().unwrap();
    let broker = Arc::new(Broker::default());
    let ark = app(
        dir.path(),
        Arc::new(Tools::default()),
        broker.clone(),
        vec![step(vec![call("test.connect", json!({"offer":"sample"}))])],
    );
    let (mut session, id) = paused(&ark).await;
    broker.ready.store(true, Ordering::SeqCst);
    let acquired = ark.engine().refresh_connection_request(&id).await.unwrap();
    let run = ark
        .engine()
        .admit_connection(&id, acquired.local_revision)
        .unwrap();
    ark.engine().start_connection(&run).unwrap();
    let winner = ark.engine().connection_request(&id).unwrap();
    assert!(ark
        .resume_connection(&mut session, run.clone(), TurnOptions::default(), |_| {})
        .await
        .is_err());
    assert_eq!(
        ark.engine().connection_request(&id).unwrap().phase,
        winner.phase
    );
    ark.engine()
        .connection_control(session.id().as_str(), true)
        .unwrap();
    session = ark.open_session(session.id().as_str()).unwrap().unwrap();
    session
        .begin_turn(ark.engine(), "Keep the replacement instruction")
        .unwrap();
    let before = ark
        .engine()
        .store
        .list_session_turns(session.id().as_str(), None)
        .unwrap();
    ark.engine().interrupt_connection(&run).unwrap();
    assert_eq!(
        ark.engine()
            .store
            .list_session_turns(session.id().as_str(), None)
            .unwrap(),
        before
    );
}

#[tokio::test]
async fn admitted_busy_and_stop_preserve_original_and_refuse_start() {
    let dir = tempfile::tempdir().unwrap();
    let tools = Arc::new(Tools::default());
    let broker = Arc::new(Broker::default());
    let ark = app(
        dir.path(),
        tools,
        broker.clone(),
        vec![step(vec![call("test.connect", json!({"offer":"sample"}))])],
    );
    let (session, id) = paused(&ark).await;
    broker.ready.store(true, Ordering::SeqCst);
    let request = ark.engine().refresh_connection_request(&id).await.unwrap();
    let run = ark
        .engine()
        .admit_connection(&id, request.local_revision)
        .unwrap();
    let mut other = ark.open_session(session.id().as_str()).unwrap().unwrap();
    assert!(other.begin_turn(ark.engine(), "new draft").is_err());
    assert_eq!(other.turn_count(), 1);
    ark.engine()
        .connection_control(session.id().as_str(), true)
        .unwrap();
    assert!(ark.engine().start_connection(&run).is_err());
    let turns = session.transcript(ark.engine(), None).unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].input, "Make the requested change with Sample");
}

#[test]
fn broker_revisions_are_decimal_strings_without_javascript_rounding() {
    let value = json!({"scopeRef":"s","brokerRevision":"9007199254740993","expiresAt":"2030-01-01T00:00:00Z"});
    let scope: PreparedConnectionScope = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(scope.broker_revision, 9_007_199_254_740_993);
    assert_eq!(serde_json::to_value(scope).unwrap(), value);
    assert!(serde_json::from_value::<PreparedConnectionScope>(
        json!({"scopeRef":"s","brokerRevision":1,"expiresAt":"x"})
    )
    .is_err());
}

struct NamedSource(&'static str);
#[async_trait::async_trait]
impl AcquisitionProvider for NamedSource {
    fn knows(&self, need: &str) -> bool {
        need == self.0
    }
    async fn find(&self, _: &str) -> String {
        self.0.into()
    }
    async fn propose(&self, _: &str) -> String {
        self.0.into()
    }
    async fn acquire(&self, _: &str, _: bool) -> AcquisitionResult {
        AcquisitionResult::info(self.0)
    }
}

#[tokio::test]
async fn additive_acquisition_keeps_defaults_and_respects_profile_override() {
    let defaults_dir = tempfile::tempdir().unwrap();
    let defaults = Ark::builder()
        .store_root(defaults_dir.path())
        .connections(true)
        .processes(true)
        .acquisition_source(Arc::new(Source))
        .provider(Arc::new(MockProvider::new("fixture", vec![])))
        .build()
        .unwrap();
    let source = defaults.profile().acquisition.as_ref().unwrap();
    assert!(source.knows("mail"));
    assert!(source.find("mail").await.contains("not connected"));
    assert!(source.knows("node"));
    assert!(source.knows("sample"));
    assert!(source
        .contextual_acquisition(&Tools::default().actions()[1], &json!({"offer":"sample"}))
        .await
        .unwrap()
        .is_some());

    let override_dir = tempfile::tempdir().unwrap();
    let overridden = Ark::builder()
        .store_root(override_dir.path())
        .connections(true)
        .processes(true)
        .profile(
            AgentProfile::builder()
                .acquisition(Arc::new(NamedSource("override")))
                .build(),
        )
        .acquisition_source(Arc::new(Source))
        .provider(Arc::new(MockProvider::new("fixture", vec![])))
        .build()
        .unwrap();
    let source = overridden.profile().acquisition.as_ref().unwrap();
    assert_eq!(source.find("override").await, "override");
    assert!(source.knows("sample"));
    assert!(!source.knows("mail"));
    assert!(!source.knows("node"));
}

#[tokio::test]
async fn replacement_and_late_old_finalization_cannot_overwrite_new_input() {
    let dir = tempfile::tempdir().unwrap();
    let tools = Arc::new(Tools::default());
    let broker = Arc::new(Broker::default());
    let ark = app(
        dir.path(),
        tools.clone(),
        broker.clone(),
        vec![step(vec![call("test.connect", json!({"offer":"sample"}))])],
    );
    let (mut stale, id) = paused(&ark).await;
    let mut current = ark.open_session(stale.id().as_str()).unwrap().unwrap();
    current
        .begin_turn(ark.engine(), "Use the new instruction instead")
        .unwrap();
    broker.ready.store(true, Ordering::SeqCst);
    assert!(ark
        .engine()
        .refresh_connection_request(&id)
        .await
        .unwrap()
        .closed());
    let outcome = HarnessOutcome {
        awaiting_connection: None,
        answer: "old result".into(),
        cited: vec![],
        steps: 0,
        trivial: true,
        metrics: Default::default(),
        state: Default::default(),
        cancelled: false,
        interrupted: false,
        steered: vec![],
        ran_out: false,
    };
    assert!(stale
        .record_turn(ark.engine(), "stale input", &outcome)
        .is_err());
    let turns = ark
        .engine()
        .store
        .list_session_turns(current.id().as_str(), None)
        .unwrap();
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0]["input"], "Make the requested change with Sample");
    assert_eq!(turns[1]["input"], "Use the new instruction instead");
    assert_eq!(tools.connects.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn counted_started_turn_recovers_in_place_without_automatic_model_replay() {
    let dir = tempfile::tempdir().unwrap();
    let tools = Arc::new(Tools::default());
    let broker = Arc::new(Broker::default());
    let ark = app(
        dir.path(),
        tools.clone(),
        broker.clone(),
        vec![step(vec![call("test.connect", json!({"offer":"sample"}))])],
    );
    let (mut session, id) = paused(&ark).await;
    broker.ready.store(true, Ordering::SeqCst);
    let pending = ark.engine().refresh_connection_request(&id).await.unwrap();
    let run = ark
        .engine()
        .admit_connection(&id, pending.local_revision)
        .unwrap();
    let turn = ark.engine().start_connection(&run).unwrap();
    assert_eq!(turn.pid, Some(std::process::id()));
    session.resume_waiting(ark.engine(), turn).unwrap();
    session
        .steered_now(ark.engine(), "Use the corrected branch")
        .unwrap();
    let mut turn = session.transcript(ark.engine(), None).unwrap().remove(0);
    let mut request = ark.engine().connection_request(&id).unwrap();
    request.phase = ConnectionRequestPhase::Started {
        run_id: "dead-run".into(),
        pid: std::process::id(),
        pid_start: Some(u64::MAX),
    };
    turn.connection = Some(TurnConnection::Started {
        request_id: id.clone(),
        run_id: "dead-run".into(),
        pid: std::process::id(),
        pid_start: Some(u64::MAX),
    });
    ark.engine()
        .store
        .put_record(
            RECORD_KIND,
            &id,
            session.id().as_str(),
            &serde_json::to_value(request).unwrap(),
        )
        .unwrap();
    ark.engine()
        .store
        .put_session_turn(
            session.id().as_str(),
            0,
            &serde_json::to_value(turn).unwrap(),
        )
        .unwrap();
    assert_eq!(ark.engine().recover_connection_requests(None).unwrap(), 1);
    let reopened = ark.open_session(session.id().as_str()).unwrap().unwrap();
    assert_eq!(reopened.turn_count(), 1);
    let turns = reopened.transcript(ark.engine(), None).unwrap();
    assert_eq!(turns.len(), 1);
    assert!(turns[0].connection.is_none());
    assert!(turns[0].incomplete);
    assert_eq!(turns[0].steered, ["Use the corrected branch"]);
    assert!(reopened
        .unfinished(ark.engine())
        .unwrap()
        .unwrap()
        .goal
        .follow_ups
        .iter()
        .any(|message| message == "Use the corrected branch"));
    assert!(matches!(
        ark.engine().connection_request(&id).unwrap().phase,
        ConnectionRequestPhase::Interrupted { .. }
    ));
    assert_eq!(tools.connects.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn rollback_dropped_or_mismatched_markers_retire_without_dispatch() {
    for changed in [
        "turn_missing",
        "turn_mismatch",
        "invocation_missing",
        "ok",
        "failed",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let tools = Arc::new(Tools::default());
        let broker = Arc::new(Broker::default());
        let ark = app(
            dir.path(),
            tools.clone(),
            broker.clone(),
            vec![step(vec![call("test.connect", json!({"offer":"sample"}))])],
        );
        let (session, id) = paused(&ark).await;
        let request = ark.engine().refresh_connection_request(&id).await.unwrap();
        if changed.starts_with("turn_") {
            let mut turn = ark
                .engine()
                .store
                .list_session_turns(session.id().as_str(), Some(1))
                .unwrap()
                .remove(0);
            if changed == "turn_missing" {
                turn.as_object_mut().unwrap().remove("connection");
            } else {
                turn["connection"]["request_id"] = json!("another-request");
            }
            ark.engine()
                .store
                .put_session_turn(session.id().as_str(), 0, &turn)
                .unwrap();
        } else {
            let mut invocation = ark
                .engine()
                .store
                .get_invocation(&request.project, &request.invocation)
                .unwrap()
                .unwrap();
            match changed {
                "invocation_missing" => invocation.connection_request = None,
                "ok" => invocation.mark_ok(json!({})),
                "failed" => invocation.mark_failed("old writer closed the call"),
                _ => unreachable!(),
            }
            ark.engine()
                .store
                .put_invocation(&request.project, &invocation)
                .unwrap();
        }
        broker.ready.store(true, Ordering::SeqCst);
        assert!(
            ark.engine()
                .refresh_connection_request(&id)
                .await
                .unwrap()
                .closed(),
            "{changed}"
        );
        assert_eq!(tools.connects.load(Ordering::SeqCst), 0, "{changed}");
        assert_eq!(broker.claims.load(Ordering::SeqCst), 0, "{changed}");
    }
}

#[tokio::test]
async fn rollback_fixture_paused_and_claimed_use_legacy_invocation_tags() {
    for phase in ["paused", "claimed"] {
        let dir = tempfile::Builder::new()
            .prefix(&format!("ark-rollback-{phase}-"))
            .tempdir()
            .unwrap();
        let tools = Arc::new(Tools::default());
        let broker = Arc::new(Broker::default());
        let ark = app(
            dir.path(),
            tools.clone(),
            broker.clone(),
            vec![step(vec![call("test.connect", json!({"offer":"sample"}))])],
        );
        let (session, id) = paused(&ark).await;
        let mut request = ark.engine().refresh_connection_request(&id).await.unwrap();
        if phase == "claimed" {
            broker.ready.store(true, Ordering::SeqCst);
            let binding = broker
                .claim(ClaimConnectionScope {
                    scope_ref: request.scope.as_ref().unwrap().scope_ref.clone(),
                    request_id: id.clone(),
                    invocation_id: request.invocation.clone(),
                    expected_broker_revision: 9_007_199_254_740_993,
                })
                .await
                .unwrap();
            request.phase = ConnectionRequestPhase::Claimed { binding };
            request.local_revision += 1;
            ark.engine()
                .store
                .put_record(
                    RECORD_KIND,
                    &id,
                    session.id().as_str(),
                    &serde_json::to_value(&request).unwrap(),
                )
                .unwrap();
        }
        let calls = ark
            .engine()
            .store
            .list_invocations(&request.project)
            .unwrap();
        assert_eq!(calls.len(), 1);
        let encoded = serde_json::to_value(&calls[0]).unwrap();
        assert_eq!(encoded["status"], "pending_approval");
        assert_eq!(encoded["connection_request"], id);
        assert_eq!(tools.connects.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn rollback_returned_root_recovery_preserves_owned_or_retires_orphan() {
    let temporary = tempfile::tempdir().unwrap();
    let external = std::env::var_os("ARK_ROLLBACK_RETURNED_ROOT").map(std::path::PathBuf::from);
    let (root, session_id, request_id) = if let Some(root) = external {
        assert!(root.is_absolute() && root.join("meta.db").is_file());
        (
            root,
            std::env::var("ARK_ROLLBACK_RETURNED_SESSION_ID").expect("exact returned session ID"),
            std::env::var("ARK_ROLLBACK_RETURNED_REQUEST_ID").unwrap_or_default(),
        )
    } else {
        let ark = app(
            temporary.path(),
            Arc::new(Tools::default()),
            Arc::new(Broker::default()),
            vec![step(vec![call("test.connect", json!({"offer":"sample"}))])],
        );
        let (session, request_id) = paused(&ark).await;
        let session_id = session.id().to_string();
        drop(session);
        drop(ark);
        (temporary.path().to_path_buf(), session_id, request_id)
    };
    let store = ark_store::Store::open(&root).unwrap();
    if request_id.is_empty() {
        let header = store.get_session(&session_id).unwrap().unwrap();
        let project = ark_core::ProjectId(header["project"].as_str().unwrap().into());
        let turns = store.list_session_turns(&session_id, None).unwrap();
        let trace = store.get_turn_trace(&session_id, 0).unwrap();
        assert!(store
            .list_records(RECORD_KIND, Some(&session_id))
            .unwrap()
            .is_empty());
        assert!(store.list_invocations(&project).unwrap().is_empty());
        let engine = Engine::new(store, Registry::new());
        assert!(!engine.has_connection_requests());
        assert!(engine.tools().actions().is_empty());
        engine
            .recover_connection_requests(Some(&session_id))
            .unwrap();
        assert_eq!(
            engine.store.get_session(&session_id).unwrap().unwrap(),
            header
        );
        assert_eq!(
            engine.store.list_session_turns(&session_id, None).unwrap(),
            turns
        );
        assert_eq!(engine.store.get_turn_trace(&session_id, 0).unwrap(), trace);
        assert!(engine.connection_requests(&session_id).unwrap().is_empty());
        assert!(engine.store.list_invocations(&project).unwrap().is_empty());
        let proof = json!({"session":session_id,"boundary":"live_journal_before_request_commit","headerUnchanged":true,"turnsUnchanged":true,
            "traceUnchanged":true,"requestCount":0,"invocationCount":0,"providerConfigured":false,"brokerConfigured":false,"adaptersRegistered":0});
        std::fs::write(
            root.join("rollback-new-reader-proof.json"),
            serde_json::to_vec_pretty(&proof).unwrap(),
        )
        .unwrap();
        println!("RETURNED_ROLLBACK_ROOT root={} session={session_id} boundary=live_journal request=none dispatch=false",root.display());
        return;
    }
    let before: PendingConnectionRequest =
        serde_json::from_value(store.get_record(RECORD_KIND, &request_id).unwrap().unwrap())
            .unwrap();
    assert_eq!(before.session, session_id);
    let header = store.get_session(&session_id).unwrap().unwrap();
    let turns = store.list_session_turns(&session_id, None).unwrap();
    let trace = store.get_turn_trace(&session_id, before.turn).unwrap();
    let calls = store.list_invocations(&before.project).unwrap();
    assert!(calls.iter().all(|call| call.effect.is_none()));
    let invocation = calls
        .iter()
        .find(|call| call.id.as_str() == before.invocation)
        .unwrap();
    let turn: ark_runtime::session::Turn = serde_json::from_value(
        turns
            .iter()
            .find(|turn| turn["index"].as_u64() == Some(u64::from(before.turn)))
            .unwrap()
            .clone(),
    )
    .unwrap();
    let owned = matches!(&turn.connection, Some(TurnConnection::Waiting { request_id }) if request_id == &before.id)
        && matches!(
            before.phase,
            ConnectionRequestPhase::Preparing
                | ConnectionRequestPhase::Waiting
                | ConnectionRequestPhase::Claimed { .. }
        )
        && invocation.status == ark_core::InvocationStatus::PendingApproval
        && invocation.connection_request.as_deref() == Some(before.id.as_str())
        && invocation.work.as_deref() == Some(before.work.as_str())
        && header["turn_count"].as_u64() == Some(u64::from(before.turn) + 1);
    let engine = Engine::new(store, Registry::new());
    assert!(!engine.has_connection_requests());
    assert!(engine.tools().actions().is_empty());
    engine
        .recover_connection_requests(Some(&session_id))
        .unwrap();
    let after = engine.connection_request(&request_id).unwrap();
    if owned {
        assert_eq!(after.phase, before.phase);
        assert_eq!(after.local_revision, before.local_revision);
    } else {
        assert!(
            matches!(after.phase, ConnectionRequestPhase::Closed { ref reason } if reason == "replaced")
        );
    }
    assert_eq!(after.metadata_retry, before.metadata_retry);
    assert_eq!(
        engine.store.get_session(&session_id).unwrap().unwrap(),
        header
    );
    assert_eq!(
        engine.store.list_session_turns(&session_id, None).unwrap(),
        turns
    );
    assert_eq!(
        engine
            .store
            .get_turn_trace(&session_id, before.turn)
            .unwrap(),
        trace
    );
    assert_eq!(
        serde_json::to_value(engine.store.list_invocations(&before.project).unwrap()).unwrap(),
        serde_json::to_value(calls).unwrap()
    );
    let proof = json!({"session":session_id,"request":request_id,"ownedMarkersPreserved":owned,"beforePhase":before.phase,"afterPhase":after.phase,
        "turnCountUnchanged":true,"originalInputUnchanged":true,"invocationsUnchanged":true,"effectsAbsent":true,"traceUnchanged":true,
        "metadataRetryUnchanged":true,"metadataRetry":before.metadata_retry,
        "providerConfigured":false,"brokerConfigured":false,"adaptersRegistered":0,"networkCalls":0});
    std::fs::write(
        root.join("rollback-new-reader-proof.json"),
        serde_json::to_vec_pretty(&proof).unwrap(),
    )
    .unwrap();
    println!("RETURNED_ROLLBACK_ROOT root={} session={session_id} request={request_id} owned={owned} dispatch=false", root.display());
}

#[tokio::test]
async fn rollback_stopped_returned_root_clears_dead_owner_without_revival() {
    let temporary = tempfile::tempdir().unwrap();
    let inputs = [
        "ARK_STOPPED_ROLLBACK_RETURNED_ROOT",
        "ARK_STOPPED_ROLLBACK_RETURNED_SESSION_ID",
        "ARK_STOPPED_ROLLBACK_RETURNED_REQUEST_ID",
    ]
    .map(std::env::var_os);
    assert!(
        inputs.iter().all(Option::is_none) || inputs.iter().all(Option::is_some),
        "all stopped return-reader environment inputs are required together"
    );
    let external = inputs.iter().all(Option::is_some);
    let (root, session_id, request_id) = if let [Some(root), Some(session), Some(request)] = inputs
    {
        let root = std::path::PathBuf::from(root);
        assert!(root.is_absolute() && root.join("meta.db").is_file());
        (
            root,
            session.into_string().expect("UTF-8 exact session ID"),
            request.into_string().expect("UTF-8 exact request ID"),
        )
    } else {
        let tools = Arc::new(Tools::default());
        let broker = Arc::new(Broker::default());
        let ark = app(
            temporary.path(),
            tools,
            broker.clone(),
            vec![step(vec![call("test.connect", json!({"offer":"sample"}))])],
        );
        let (session, request_id) = started(&ark, &broker).await;
        ark.control_session(session.id().as_str(), true).unwrap();
        let mut row = ark
            .engine()
            .store
            .list_session_turns(session.id().as_str(), None)
            .unwrap()
            .remove(0);
        row["connection"]["pid_start"] = json!(u64::MAX);
        ark.engine()
            .store
            .put_session_turn(session.id().as_str(), 0, &row)
            .unwrap();
        (
            temporary.path().to_path_buf(),
            session.id().to_string(),
            request_id,
        )
    };
    let proof_path = root.join("rollback-stopped-new-reader-proof.json");
    assert!(
        !proof_path.exists(),
        "proof must not overwrite existing evidence"
    );
    let credentials = || -> std::collections::BTreeMap<String, Vec<u8>> {
        ["access.json", "secrets.enc"]
            .into_iter()
            .filter_map(|name| {
                let path = root.join(name);
                path.is_file()
                    .then(|| (name.to_string(), std::fs::read(path).unwrap()))
            })
            .collect()
    };
    let before_credentials = credentials();
    if external {
        assert!(
            before_credentials.contains_key("access.json")
                && before_credentials.contains_key("secrets.enc"),
            "external stopped fixture requires both credential files"
        );
    }
    let store = ark_store::Store::open(&root).unwrap();
    let request_json = store.get_record(RECORD_KIND, &request_id).unwrap().unwrap();
    let request: PendingConnectionRequest = serde_json::from_value(request_json.clone()).unwrap();
    assert_eq!(request.id, request_id);
    assert_eq!(request.session, session_id);
    assert!(
        matches!(&request.phase, ConnectionRequestPhase::Closed { reason } if reason == "cancelled")
    );
    let header = store.get_session(&session_id).unwrap().unwrap();
    assert_eq!(header["id"], session_id);
    assert_eq!(header["project"], request.project.as_str());
    assert_eq!(header["turn_count"], 1);
    assert_eq!(
        u64::from(request.turn) + 1,
        header["turn_count"].as_u64().unwrap()
    );
    assert_eq!(
        request.work,
        format!("session:{session_id}/{}", request.turn)
    );
    let rows = store.list_session_turns(&session_id, None).unwrap();
    assert_eq!(rows.len(), 1);
    let original: ark_runtime::session::Turn = serde_json::from_value(rows[0].clone()).unwrap();
    assert_eq!(original.index, request.turn);
    assert!(original.cancelled && original.pid.is_none() && original.pid_start.is_none());
    let Some(TurnConnection::Started {
        request_id: owner,
        run_id,
        pid,
        pid_start,
    }) = &original.connection
    else {
        panic!("exact stopped Started marker required");
    };
    assert_eq!(owner, &request_id);
    assert!(!run_id.is_empty());
    assert!(
        !ark_runtime::process::writer_alive(*pid, *pid_start),
        "returned fixture writer must be dead"
    );
    let trace = store.get_turn_trace(&session_id, request.turn).unwrap();
    let invocations =
        serde_json::to_value(store.list_invocations(&request.project).unwrap()).unwrap();
    assert_eq!(invocations.as_array().unwrap().len(), 1);
    assert!(invocations
        .as_array()
        .unwrap()
        .iter()
        .all(|invocation| invocation["effect"].is_null()));
    let engine = Engine::new(store, Registry::new());
    assert!(!engine.has_connection_requests());
    assert!(engine.tools().actions().is_empty());
    assert_eq!(
        engine
            .recover_connection_requests(Some(&session_id))
            .unwrap(),
        1
    );
    let mut expected = rows[0].clone();
    expected.as_object_mut().unwrap().remove("connection");
    assert_eq!(
        engine.store.list_session_turns(&session_id, None).unwrap(),
        [expected.clone()]
    );
    assert_eq!(
        engine.store.get_record(RECORD_KIND, &request_id).unwrap(),
        Some(request_json.clone())
    );
    assert_eq!(
        engine.store.get_session(&session_id).unwrap(),
        Some(header.clone())
    );
    assert_eq!(
        engine
            .store
            .get_turn_trace(&session_id, request.turn)
            .unwrap(),
        trace
    );
    assert_eq!(
        serde_json::to_value(engine.store.list_invocations(&request.project).unwrap()).unwrap(),
        invocations
    );
    assert!(engine
        .admit_connection(&request_id, request.local_revision)
        .is_err());
    for _ in 0..2 {
        let reopened = Session::open(&engine, &session_id).unwrap().unwrap();
        assert_eq!(reopened.turn_count(), 1);
        assert_eq!(
            reopened.transcript(&engine, None).unwrap()[0].input,
            original.input
        );
        assert_eq!(
            engine
                .recover_connection_requests(Some(&session_id))
                .unwrap(),
            0
        );
        assert_eq!(
            engine.store.get_session(&session_id).unwrap(),
            Some(header.clone())
        );
        assert_eq!(
            engine.store.get_record(RECORD_KIND, &request_id).unwrap(),
            Some(request_json.clone())
        );
        assert_eq!(
            engine.store.list_session_turns(&session_id, None).unwrap(),
            [expected.clone()]
        );
    }
    assert_eq!(
        engine
            .store
            .get_turn_trace(&session_id, request.turn)
            .unwrap(),
        trace
    );
    assert_eq!(
        serde_json::to_value(engine.store.list_invocations(&request.project).unwrap()).unwrap(),
        invocations
    );
    assert_eq!(credentials(), before_credentials);
    let credential_content_ids: std::collections::BTreeMap<_, _> = before_credentials
        .iter()
        .map(|(name, bytes)| {
            (
                name.clone(),
                ArtifactId::of_bytes(bytes).as_str().to_string(),
            )
        })
        .collect();
    let executable = std::env::current_exe().unwrap();
    let proof = json!({"session":session_id,"request":request_id,"run":run_id,"hashAlgorithm":"blake3",
        "currentExecutableContentId":ArtifactId::of_bytes(&std::fs::read(executable).unwrap()).as_str(),
        "credentialContentIds":credential_content_ids,"headerJsonUnchanged":true,"requestJsonUnchanged":true,
        "onlyTurnChange":"removed_connection_marker","sqlSessionBookkeepingTimestampMayChange":true,
        "inputCount":1,"turnCount":1,"metricsUnchanged":true,"citationsUnchanged":true,"traceUnchanged":true,"invocationsUnchanged":true,
        "admissionRejected":true,"idempotentReopen":true,"providerConfigured":false,"brokerConfigured":false,"adaptersRegistered":0});
    assert!(
        !proof_path.exists(),
        "proof must not overwrite existing evidence"
    );
    std::fs::write(proof_path, serde_json::to_vec_pretty(&proof).unwrap()).unwrap();
}

#[tokio::test]
async fn stopped_missing_or_changed_marker_never_reconstructs_late_finalizer_ownership() {
    for changed in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let broker = Arc::new(Broker::default());
        let ark = app(
            dir.path(),
            Arc::new(Tools::default()),
            broker.clone(),
            vec![step(vec![call("test.connect", json!({"offer":"sample"}))])],
        );
        let (mut session, id) = started(&ark, &broker).await;
        ark.control_session(session.id().as_str(), true).unwrap();
        let mut row = ark
            .engine()
            .store
            .list_session_turns(session.id().as_str(), None)
            .unwrap()
            .remove(0);
        if changed {
            row["connection"]["run_id"] = json!("different-owner");
        } else {
            row.as_object_mut().unwrap().remove("connection");
        }
        ark.engine()
            .store
            .put_session_turn(session.id().as_str(), 0, &row)
            .unwrap();
        let header = ark
            .engine()
            .store
            .get_session(session.id().as_str())
            .unwrap();
        let request = ark.engine().store.get_record(RECORD_KIND, &id).unwrap();
        assert!(session
            .record_turn(ark.engine(), "", &partial_outcome())
            .is_err());
        assert_eq!(
            ark.engine()
                .store
                .list_session_turns(session.id().as_str(), None)
                .unwrap(),
            [row]
        );
        assert_eq!(
            ark.engine()
                .store
                .get_session(session.id().as_str())
                .unwrap(),
            header
        );
        assert_eq!(
            ark.engine().store.get_record(RECORD_KIND, &id).unwrap(),
            request
        );
    }
}
