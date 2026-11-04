use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, Mutex,
};
use std::time::Duration;

use ark_interact::App;
use ark_model::{Completion, MockProvider, ToolCall, Usage};
use ark_runtime::connection_requests::*;
use ark_sdk::prelude::*;
use ark_tools::ClaimedConnectionBinding;
use ark_ui::{access::Tokens, ArkUi, ClientListener};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

#[derive(Default)]
struct Connect(
    AtomicUsize,
    AtomicBool,
    AtomicUsize,
    AtomicBool,
    AtomicUsize,
);

struct ActiveCall<'a>(&'a AtomicUsize);
impl Drop for ActiveCall<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
#[async_trait::async_trait]
impl Adapter for Connect {
    fn name(&self) -> &str {
        "fixture"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![
            ActionDescriptor::new("fixture.connect", "Connect Sample", RiskTier::Observe)
                .contextual_acquisition(),
        ]
    }
    async fn run(&self, _: &str, _: Value, ctx: &RunCtx<'_>) -> ark_tools::Result<ActionOutput> {
        self.2.fetch_add(1, Ordering::SeqCst);
        self.4.fetch_add(1, Ordering::SeqCst);
        let _active = ActiveCall(&self.4);
        if self.3.load(Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        let binding = ctx.connection_binding.as_ref().expect("trusted claim");
        assert_eq!(
            ctx.invocation.as_deref(),
            Some(binding.invocation_id.as_str())
        );
        if self.1.load(Ordering::SeqCst) {
            return Err(AdapterError::Tool(
                "fixture stops after durable claim".into(),
            ));
        }
        tokio::task::yield_now().await;
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(ActionOutput::json(json!({"bindingId": binding.binding_id})))
    }
}
struct Source;
#[async_trait::async_trait]
impl AcquisitionProvider for Source {
    async fn find(&self, _: &str) -> String {
        String::new()
    }
    async fn propose(&self, _: &str) -> String {
        String::new()
    }
    async fn acquire(&self, _: &str, _: bool) -> AcquisitionResult {
        AcquisitionResult::info("unused")
    }
    async fn contextual_acquisition(
        &self,
        action: &ActionDescriptor,
        _: &Value,
    ) -> ConnectionResult<Option<ContextualAcquisition>> {
        Ok(
            (action.name == "fixture.connect").then(|| ContextualAcquisition {
                offer_id: "sample".into(),
                display: "Connect Sample".into(),
            }),
        )
    }
}
#[derive(Default)]
struct Broker {
    ready: AtomicBool,
    cancelled: AtomicBool,
    claim: Mutex<Option<ClaimedConnectionBinding>>,
    resolves: AtomicUsize,
    unavailable: AtomicBool,
    refused: AtomicBool,
    hang: AtomicBool,
    active: AtomicUsize,
    peak: AtomicUsize,
    hang_operation: Mutex<Option<&'static str>>,
    entered: tokio::sync::Notify,
    resolved: tokio::sync::Notify,
}

impl Broker {
    async fn pause_if(&self, operation: &'static str) {
        let pause = *self.hang_operation.lock().unwrap() == Some(operation);
        if pause {
            self.entered.notify_one();
            std::future::pending::<()>().await;
        }
    }
}
#[async_trait::async_trait]
impl ConnectionRequestBroker for Broker {
    fn route_id(&self) -> &str {
        "fixture-route"
    }
    async fn prepare(
        &self,
        _: ConnectionScopeRequest,
    ) -> ConnectionResult<PreparedConnectionScope> {
        self.pause_if("prepare").await;
        Ok(PreparedConnectionScope {
            scope_ref: "scope".into(),
            broker_revision: 1,
            expires_at: "2030-01-01T00:00:00Z".into(),
        })
    }
    async fn resolve(
        &self,
        input: ResolveConnectionScope,
    ) -> ConnectionResult<ResolvedConnectionScope> {
        self.resolves.fetch_add(1, Ordering::SeqCst);
        self.resolved.notify_one();
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(active, Ordering::SeqCst);
        let _active = ActiveCall(&self.active);
        self.pause_if("resolve").await;
        if self.hang.load(Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        if self.refused.load(Ordering::SeqCst) {
            return Err(ConnectionRequestError::Refused("fixture refusal".into()));
        }
        if self.unavailable.load(Ordering::SeqCst) {
            return Err(ConnectionRequestError::Unavailable(
                "fixture broker outage".into(),
            ));
        }
        let receipt = self.claim.lock().unwrap().clone();
        Ok(ResolvedConnectionScope {
            scope_ref: input.scope_ref,
            request_id: input.request_id,
            invocation_id: input.invocation_id,
            route_id: input.route_id,
            offer_id: "sample".into(),
            fingerprint: "schema".into(),
            broker_revision: 1 + u64::from(receipt.is_some()),
            state: if self.cancelled.load(Ordering::SeqCst) {
                ConnectionScopeState::Cancelled
            } else if receipt.is_some() {
                ConnectionScopeState::Claimed
            } else if self.ready.load(Ordering::SeqCst) {
                ConnectionScopeState::Ready
            } else {
                ConnectionScopeState::Pending
            },
            claim_receipt: receipt,
        })
    }
    async fn claim(
        &self,
        input: ClaimConnectionScope,
    ) -> ConnectionResult<ClaimedConnectionBinding> {
        self.pause_if("claim").await;
        assert!(!self.cancelled.load(Ordering::SeqCst));
        Ok(self
            .claim
            .lock()
            .unwrap()
            .get_or_insert_with(|| ClaimedConnectionBinding {
                receipt_id: "receipt".into(),
                scope_ref: input.scope_ref,
                request_id: input.request_id,
                invocation_id: input.invocation_id,
                route_id: self.route_id().into(),
                offer_id: "sample".into(),
                binding_id: "binding".into(),
                fingerprint: "schema".into(),
                broker_revision: 2,
            })
            .clone())
    }
    async fn cancel(
        &self,
        input: ClaimConnectionScope,
    ) -> ConnectionResult<ResolvedConnectionScope> {
        self.pause_if("cancel").await;
        self.cancelled.store(true, Ordering::SeqCst);
        self.resolve(ResolveConnectionScope {
            scope_ref: input.scope_ref,
            request_id: input.request_id,
            invocation_id: input.invocation_id,
            route_id: self.route_id().into(),
        })
        .await
    }
}

async fn fixture(path: &std::path::Path) -> (Ark, Session, String, Arc<Broker>, Arc<Connect>) {
    let broker = Arc::new(Broker::default());
    let tool = Arc::new(Connect::default());
    let ark = Ark::builder()
        .store_root(path)
        .tool(tool.clone())
        .connection_requests(broker.clone())
        .profile(
            AgentProfile::builder()
                .core_actions(["fixture.connect"])
                .acquisition(Arc::new(Source))
                .build(),
        )
        .provider(Arc::new(MockProvider::new(
            "fixture",
            vec![
                Completion {
                    content: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "connect-call".into(),
                        name: "fixture.connect".into(),
                        arguments: json!({"offer":"sample"}),
                    }],
                    usage: Usage::default(),
                },
                Completion {
                    content: "Connected.".into(),
                    tool_calls: vec![],
                    usage: Usage::default(),
                },
            ],
        )))
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark
        .find_or_create_project("Sample", path.to_str().unwrap())
        .unwrap();
    let mut session = ark.start_session(&project.id).unwrap();
    let outcome = ark
        .ask_session(
            &mut session,
            "Keep this exact request",
            TurnOptions::default(),
            |_| {},
        )
        .await
        .unwrap();
    let id = outcome.awaiting_connection.unwrap().request_id;
    (ark, session, id, broker, tool)
}
async fn raw(port: u16, request: String) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut socket = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    socket.write_all(request.as_bytes()).await.unwrap();
    let mut result = String::new();
    socket.read_to_string(&mut result).await.unwrap();
    result
}
fn get(path: &str, auth: &str) -> String {
    format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n{auth}\r\n")
}
fn post(path: &str, auth: &str, body: &Value) -> String {
    let body = body.to_string();
    format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n{auth}Content-Length: {}\r\n\r\n{body}",
        body.len()
    )
}
fn body(response: &str) -> Value {
    serde_json::from_str(response.split("\r\n\r\n").nth(1).unwrap()).unwrap()
}

async fn snapshot_with_state(port: u16, path: &str, auth: &str, state: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let response = raw(port, get(path, auth)).await;
            assert!(response.starts_with("HTTP/1.1 200"), "{response}");
            let snapshot = body(&response);
            if snapshot["connectionRequests"][0]["state"] == state {
                return snapshot;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap()
}
fn mutation(epoch: &str, operation: &str, target: &str, input: Value) -> Value {
    json!({"epoch":epoch,"counter":"1","operation":operation,"target":target,"inputDigest":format!("{:x}",Sha256::digest(serde_json::to_vec(&input).unwrap())),"input":input})
}
async fn serve(
    ark: &Ark,
    path: &std::path::Path,
) -> (Arc<ark_ui::Host>, ark_ui::ClientServer, String) {
    let tokens = Tokens::in_store(path);
    let phone = tokens
        .issue_external_client(
            "Account",
            "Phone",
            "connection-fixture",
            Duration::from_secs(3600),
        )
        .unwrap();
    let (host, _) = ArkUi::new(App::new("Fixture").ark(ark.clone()))
        .open()
        .await
        .unwrap();
    let served = ClientListener::new(tokens)
        .route_id("fixture-route")
        .unwrap()
        .serve(host.clone(), "127.0.0.1:0")
        .await
        .unwrap();
    (
        host,
        served,
        format!("Authorization: Bearer {}\r\n", phone.token),
    )
}

#[tokio::test(start_paused = true)]
async fn stored_gets_do_not_poll_broker_and_fast_failures_back_off() {
    let keep_clock_paused = tokio::spawn(async {
        loop {
            tokio::task::yield_now().await;
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let (ark, session, _, broker, _) = fixture(dir.path()).await;
    broker.unavailable.store(true, Ordering::SeqCst);
    let (_, served, auth) = serve(&ark, dir.path()).await;
    broker.resolved.notified().await;
    snapshot_with_state(
        served.port,
        &format!("/client/v1/chats/{}", session.id()),
        &auth,
        "uncertain",
    )
    .await;
    let attempts = broker.resolves.load(Ordering::SeqCst);
    for _ in 0..8 {
        let response = raw(
            served.port,
            get(&format!("/client/v1/chats/{}", session.id()), &auth),
        )
        .await;
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    }
    assert_eq!(broker.resolves.load(Ordering::SeqCst), attempts);
    served.shutdown().await.unwrap();
    keep_clock_paused.abort();
}

#[tokio::test(start_paused = true)]
async fn maintenance_fast_failure_poll_deadlines_are_one_then_two_seconds() {
    let keep_clock_paused = tokio::spawn(async {
        loop {
            tokio::task::yield_now().await;
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let (ark, _, _, broker, _) = fixture(dir.path()).await;
    broker.unavailable.store(true, Ordering::SeqCst);
    let (_, served, _) = serve(&ark, dir.path()).await;
    broker.resolved.notified().await;
    assert_eq!(broker.resolves.load(Ordering::SeqCst), 1);
    tokio::time::advance(Duration::from_millis(750)).await;
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    assert_eq!(broker.resolves.load(Ordering::SeqCst), 1);
    tokio::time::advance(Duration::from_millis(250)).await;
    broker.resolved.notified().await;
    assert_eq!(broker.resolves.load(Ordering::SeqCst), 2);
    tokio::time::advance(Duration::from_millis(1750)).await;
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    assert_eq!(broker.resolves.load(Ordering::SeqCst), 2);
    tokio::time::advance(Duration::from_millis(250)).await;
    broker.resolved.notified().await;
    assert_eq!(broker.resolves.load(Ordering::SeqCst), 3);
    served.shutdown().await.unwrap();
    keep_clock_paused.abort();
}

#[tokio::test]
async fn hanging_broker_does_not_block_reads_stop_or_listener_shutdown_and_shared_permit_reuse() {
    for graceful in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let (ark, session, id, broker, _) = fixture(dir.path()).await;
        for _ in 0..5 {
            fixture(dir.path()).await;
        }
        broker.hang.store(true, Ordering::SeqCst);
        let (host, served, auth) =
            tokio::time::timeout(Duration::from_secs(2), serve(&ark, dir.path()))
                .await
                .unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while broker.active.load(Ordering::SeqCst) != 4 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let concurrent = ClientListener::new(Tokens::in_store(dir.path()))
            .route_id("fixture-route")
            .unwrap()
            .serve(host.clone(), "127.0.0.1:0")
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(broker.peak.load(Ordering::SeqCst), 4);
        let resolves = broker.resolves.load(Ordering::SeqCst);
        let response = tokio::time::timeout(
            Duration::from_secs(2),
            raw(
                served.port,
                get(&format!("/client/v1/chats/{}", session.id()), &auth),
            ),
        )
        .await
        .unwrap();
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert_eq!(broker.resolves.load(Ordering::SeqCst), resolves);
        tokio::time::timeout(Duration::from_secs(2), async {
            host.call("open_session", json!({"id":session.id()}))
                .await
                .unwrap();
            host.call("cancel", json!({"session":session.id()}))
                .await
                .unwrap();
        })
        .await
        .unwrap();
        assert!(ark.engine().connection_request(&id).unwrap().closed());
        concurrent.shutdown().await.unwrap();
        if graceful {
            served.shutdown().await.unwrap();
        } else {
            drop(served);
        }
        tokio::time::timeout(Duration::from_secs(2), async {
            while broker.active.load(Ordering::SeqCst) != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let next = ClientListener::new(Tokens::in_store(dir.path()))
            .route_id("fixture-route")
            .unwrap()
            .serve(host, "127.0.0.1:0")
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while broker.active.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        next.shutdown().await.unwrap();
        assert_eq!(broker.active.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn hanging_metadata_does_not_block_snapshot_or_awaited_listener_shutdown() {
    let dir = tempfile::tempdir().unwrap();
    let (ark, session, _, broker, tool) = fixture(dir.path()).await;
    broker.ready.store(true, Ordering::SeqCst);
    tool.3.store(true, Ordering::SeqCst);
    let (_, served, auth) = tokio::time::timeout(Duration::from_secs(2), serve(&ark, dir.path()))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while tool.4.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let response = tokio::time::timeout(
        Duration::from_secs(2),
        raw(
            served.port,
            get(&format!("/client/v1/chats/{}", session.id()), &auth),
        ),
    )
    .await
    .unwrap();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert_eq!(
        body(&response)["connectionRequests"][0]["state"],
        "uncertain"
    );
    served.shutdown().await.unwrap();
    assert_eq!(tool.4.load(Ordering::SeqCst), 0);
    assert_eq!(tool.0.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn all_broker_operations_and_metadata_have_ten_second_bounds() {
    let keep_clock_paused = tokio::spawn(async {
        loop {
            tokio::task::yield_now().await;
        }
    });
    for operation in ["prepare", "resolve", "claim", "cancel"] {
        let dir = tempfile::tempdir().unwrap();
        let (ark, session, id, broker, _) = fixture(dir.path()).await;
        if operation == "cancel" {
            ark.engine().refresh_connection_request(&id).await.unwrap();
            ark.engine()
                .connection_control(session.id().as_str(), true)
                .unwrap();
        }
        broker.ready.store(operation == "claim", Ordering::SeqCst);
        *broker.hang_operation.lock().unwrap() = Some(operation);
        let engine = ark.shared_engine();
        let waiting = tokio::spawn(async move { engine.refresh_connection_request(&id).await });
        broker.entered.notified().await;
        tokio::time::advance(Duration::from_secs(10)).await;
        assert!(matches!(
            waiting.await.unwrap(),
            Err(ark_runtime::AgentError::Connection(
                ConnectionRequestError::Unavailable(_)
            ))
        ));
        assert_eq!(broker.active.load(Ordering::SeqCst), 0);
    }
    let dir = tempfile::tempdir().unwrap();
    let (ark, _, id, broker, tool) = fixture(dir.path()).await;
    broker.ready.store(true, Ordering::SeqCst);
    tool.3.store(true, Ordering::SeqCst);
    let engine = ark.shared_engine();
    let waiting = tokio::spawn(async move { engine.refresh_connection_request(&id).await });
    while tool.4.load(Ordering::SeqCst) == 0 {
        tokio::task::yield_now().await;
    }
    tokio::time::advance(Duration::from_secs(10)).await;
    assert!(matches!(
        waiting.await.unwrap(),
        Err(ark_runtime::AgentError::Connection(
            ConnectionRequestError::MetadataUnavailable
        ))
    ));
    assert_eq!(tool.4.load(Ordering::SeqCst), 0);
    keep_clock_paused.abort();
}

#[tokio::test]
async fn broker_refusal_closes_unavailable_without_hiding_chat_or_rewriting_cancelled() {
    let dir = tempfile::tempdir().unwrap();
    let (ark, session, id, broker, tool) = fixture(dir.path()).await;
    broker.refused.store(true, Ordering::SeqCst);
    let (_, served, auth) = serve(&ark, dir.path()).await;
    snapshot_with_state(
        served.port,
        &format!("/client/v1/chats/{}", session.id()),
        &auth,
        "closed",
    )
    .await;
    assert!(
        matches!(ark.engine().connection_request(&id).unwrap().phase,
        ConnectionRequestPhase::Closed { reason } if reason == "unavailable")
    );
    assert_eq!(tool.0.load(Ordering::SeqCst), 0);
    served.shutdown().await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (ark, session, id, broker, _) = fixture(dir.path()).await;
    ark.engine().refresh_connection_request(&id).await.unwrap();
    ark.engine()
        .connection_control(session.id().as_str(), true)
        .unwrap();
    broker.refused.store(true, Ordering::SeqCst);
    assert!(matches!(
        ark.engine().refresh_connection_request(&id).await,
        Err(ark_runtime::AgentError::Connection(
            ConnectionRequestError::Refused(_)
        ))
    ));
    assert!(
        matches!(ark.engine().connection_request(&id).unwrap().phase,
        ConnectionRequestPhase::Closed { reason } if reason == "cancelled")
    );
}

#[tokio::test]
async fn unavailable_selected_model_never_admits_or_falls_back_on_continuation() {
    let dir = tempfile::tempdir().unwrap();
    let (ark, session, id, broker, _) = fixture(dir.path()).await;
    broker.ready.store(true, Ordering::SeqCst);
    ark.engine().refresh_connection_request(&id).await.unwrap();
    let version = ark
        .engine()
        .store
        .session_version(session.id().as_str())
        .unwrap()
        .unwrap();
    let mut header: Value = serde_json::from_str(&version).unwrap();
    header["model"] = json!("missing-fixture-provider missing-model");
    ark.engine()
        .store
        .put_session_if(session.id().as_str(), &version, &header)
        .unwrap();
    let (_, served, auth) = serve(&ark, dir.path()).await;
    snapshot_with_state(
        served.port,
        &format!("/client/v1/chats/{}", session.id()),
        &auth,
        "uncertain",
    )
    .await;
    assert!(matches!(
        ark.engine().connection_request(&id).unwrap().phase,
        ConnectionRequestPhase::Acquired { .. }
    ));
    let hello = body(&raw(served.port, get("/client/v1/hello", &auth)).await);
    let pending = ark.engine().connection_request(&id).unwrap();
    let request = mutation(
        hello["mutationEpoch"].as_str().unwrap(),
        "chat.connection.continue",
        session.id().as_str(),
        json!({"requestId":id,"expectedRevision":pending.local_revision.to_string()}),
    );
    let response = raw(served.port, post("/client/v1/mutations", &auth, &request)).await;
    assert!(response.contains("model_unavailable"), "{response}");
    assert_eq!(
        body(&raw(served.port, get("/client/v1/hello", &auth)).await)["nextMutationCounter"],
        "1"
    );
    assert!(matches!(
        ark.engine().connection_request(&id).unwrap().phase,
        ConnectionRequestPhase::Acquired { .. }
    ));
    served.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn connection_http_exact_chat_decimal_revision_auth_and_lost_response_replay() {
    let keep_clock_paused = tokio::spawn(async {
        loop {
            tokio::task::yield_now().await;
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let (ark, session, id, broker, tool) = fixture(dir.path()).await;
    ark.engine().refresh_connection_request(&id).await.unwrap();
    broker.resolved.notified().await;
    let mut pending = ark.engine().connection_request(&id).unwrap();
    pending.local_revision = 9_007_199_254_740_993;
    ark.engine()
        .store
        .put_record(
            RECORD_KIND,
            &id,
            session.id().as_str(),
            &serde_json::to_value(pending).unwrap(),
        )
        .unwrap();
    let (_, served, auth) = serve(&ark, dir.path()).await;
    broker.resolved.notified().await;
    let hello = body(&raw(served.port, get("/client/v1/hello", &auth)).await);
    assert!(hello["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .any(|value| value == "chat.connections"));
    let snapshot = body(
        &raw(
            served.port,
            get(&format!("/client/v1/chats/{}", session.id()), &auth),
        )
        .await,
    );
    assert_eq!(
        snapshot["connectionRequests"][0]["revision"],
        "9007199254740993"
    );
    broker.ready.store(true, Ordering::SeqCst);
    let acquired = ark.engine().refresh_connection_request(&id).await.unwrap();
    let request = mutation(
        hello["mutationEpoch"].as_str().unwrap(),
        "chat.connection.continue",
        session.id().as_str(),
        json!({"requestId":id,"expectedRevision":acquired.local_revision.to_string()}),
    );
    let unauthorized = raw(served.port, post("/client/v1/mutations", "", &request)).await;
    assert!(unauthorized.starts_with("HTTP/1.1 401"), "{unauthorized}");
    let first = raw(served.port, post("/client/v1/mutations", &auth, &request)).await;
    assert!(first.starts_with("HTTP/1.1 200"), "{first}");
    let replay = raw(served.port, post("/client/v1/mutations", &auth, &request)).await;
    assert_eq!(body(&first), body(&replay));
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if matches!(
                ark.engine().connection_request(&id).unwrap().phase,
                ConnectionRequestPhase::Completed
            ) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let turns = ark
        .engine()
        .store
        .list_session_turns(session.id().as_str(), None)
        .unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0]["input"], "Keep this exact request");
    assert_eq!(tool.0.load(Ordering::SeqCst), 1);
    assert_eq!(
        body(&raw(served.port, get("/client/v1/hello", &auth)).await)["nextMutationCounter"],
        "2"
    );
    served.shutdown().await.unwrap();
    keep_clock_paused.abort();
}

#[tokio::test(start_paused = true)]
async fn connection_http_busy_does_not_consume_counter_and_stop_fences_late_auth() {
    let keep_clock_paused = tokio::spawn(async {
        loop {
            tokio::task::yield_now().await;
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let (ark, session, id, broker, tool) = fixture(dir.path()).await;
    let (host, served, auth) = serve(&ark, dir.path()).await;
    while broker.resolves.load(Ordering::SeqCst) < 1 {
        tokio::task::yield_now().await;
    }
    let hello = body(&raw(served.port, get("/client/v1/hello", &auth)).await);
    broker.ready.store(true, Ordering::SeqCst);
    let pending = ark.engine().refresh_connection_request(&id).await.unwrap();
    let run = ark
        .engine()
        .admit_connection(&id, pending.local_revision)
        .unwrap();
    let send = mutation(
        hello["mutationEpoch"].as_str().unwrap(),
        "chat.send",
        session.id().as_str(),
        json!({"body":"Retain this draft"}),
    );
    for _ in 0..2 {
        let refused = raw(served.port, post("/client/v1/mutations", &auth, &send)).await;
        assert!(refused.contains("chat_busy"), "{refused}");
        assert_eq!(
            body(&raw(served.port, get("/client/v1/hello", &auth)).await)["nextMutationCounter"],
            "1"
        );
    }
    host.call("open_session", json!({"id":session.id()}))
        .await
        .unwrap();
    host.call("cancel", json!({"session":session.id()}))
        .await
        .unwrap();
    assert!(ark.engine().start_connection(&run).is_err());
    assert!(ark
        .engine()
        .refresh_connection_request(&id)
        .await
        .unwrap()
        .closed());
    let turns = ark
        .engine()
        .store
        .list_session_turns(session.id().as_str(), None)
        .unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0]["input"], "Keep this exact request");
    assert_eq!(tool.0.load(Ordering::SeqCst), 1);
    keep_clock_paused.abort();
}

#[tokio::test]
async fn connection_http_snapshot_cannot_resume_after_stop_and_late_authorization() {
    let dir = tempfile::tempdir().unwrap();
    let (ark, session, id, broker, tool) = fixture(dir.path()).await;
    let (host, served, auth) = serve(&ark, dir.path()).await;
    host.call("open_session", json!({"id":session.id()}))
        .await
        .unwrap();
    host.call("cancel", json!({"session":session.id()}))
        .await
        .unwrap();
    broker.ready.store(true, Ordering::SeqCst);
    let snapshot = raw(
        served.port,
        get(&format!("/client/v1/chats/{}", session.id()), &auth),
    )
    .await;
    assert!(snapshot.starts_with("HTTP/1.1 200"), "{snapshot}");
    assert!(ark.engine().connection_request(&id).unwrap().closed());
    assert_eq!(tool.0.load(Ordering::SeqCst), 0);
    let turns = ark
        .engine()
        .store
        .list_session_turns(session.id().as_str(), None)
        .unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0]["input"], "Keep this exact request");
}

#[tokio::test]
async fn connection_http_missing_chat_keeps_existing_terminal_receipt() {
    let dir = tempfile::tempdir().unwrap();
    let (ark, session, _, _, tool) = fixture(dir.path()).await;
    let (_, served, auth) = serve(&ark, dir.path()).await;
    let hello = body(&raw(served.port, get("/client/v1/hello", &auth)).await);
    let request = mutation(
        hello["mutationEpoch"].as_str().unwrap(),
        "chat.send",
        "missing-chat",
        json!({"body":"Do not create a replacement chat"}),
    );
    let first = raw(served.port, post("/client/v1/mutations", &auth, &request)).await;
    assert!(first.starts_with("HTTP/1.1 200"), "{first}");
    assert_eq!(body(&first)["status"], "no_longer_applicable");
    let replay = raw(served.port, post("/client/v1/mutations", &auth, &request)).await;
    assert_eq!(body(&replay), body(&first));
    assert_eq!(
        body(&raw(served.port, get("/client/v1/hello", &auth)).await)["nextMutationCounter"],
        "2"
    );
    assert_eq!(
        ark.engine()
            .store
            .list_session_turns(session.id().as_str(), None)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(tool.0.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn connection_http_concurrent_auto_resume_reads_all_return_snapshots() {
    let dir = tempfile::tempdir().unwrap();
    let (ark, session, id, broker, tool) = fixture(dir.path()).await;
    let (_, served, auth) = serve(&ark, dir.path()).await;
    broker.ready.store(true, Ordering::SeqCst);
    let mut readers = tokio::task::JoinSet::new();
    for _ in 0..12 {
        let request = get(&format!("/client/v1/chats/{}", session.id()), &auth);
        let port = served.port;
        readers.spawn(async move { raw(port, request).await });
    }
    while let Some(response) = readers.join_next().await {
        let response = response.unwrap();
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(body(&response)["connectionRequests"].is_array());
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if matches!(
                ark.engine().connection_request(&id).unwrap().phase,
                ConnectionRequestPhase::Completed
            ) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(tool.0.load(Ordering::SeqCst), 1);
    let turns = ark
        .engine()
        .store
        .list_session_turns(session.id().as_str(), None)
        .unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0]["input"], "Keep this exact request");
}

#[tokio::test]
async fn connection_http_metadata_failure_stays_readable_and_two_devices_resume_once() {
    let dir = tempfile::tempdir().unwrap();
    let (ark, session, id, broker, tool) = fixture(dir.path()).await;
    tool.1.store(true, Ordering::SeqCst);
    broker.ready.store(true, Ordering::SeqCst);
    let (_, served, auth) = serve(&ark, dir.path()).await;
    let path = format!("/client/v1/chats/{}", session.id());
    let snapshot = snapshot_with_state(served.port, &path, &auth, "uncertain").await;
    assert_eq!(snapshot["connectionRequests"][0]["state"], "uncertain");
    assert!(snapshot["connectionRequests"][0]
        .get("scopeRef")
        .is_none_or(Value::is_null));
    assert_eq!(snapshot["messages"][0]["body"], "Keep this exact request");
    let original = ark.engine().connection_request(&id).unwrap();
    let before = serde_json::to_value(&original).unwrap();
    let mut delayed = before.clone();
    delayed["metadata_retry"]["next_at"] = json!("2030-01-01T00:00:00Z");
    ark.engine()
        .store
        .compare_record(RECORD_KIND, &id, &before, &delayed)
        .unwrap();
    let attempts = tool.2.load(Ordering::SeqCst);
    let resolves = broker.resolves.load(Ordering::SeqCst);
    let second = Tokens::in_store(dir.path())
        .issue_external_client(
            "Account",
            "Second phone",
            "connection-fixture-two",
            Duration::from_secs(3600),
        )
        .unwrap();
    let second_auth = format!("Authorization: Bearer {}\r\n", second.token);
    let (first, second) = tokio::join!(
        raw(served.port, get(&path, &auth)),
        raw(served.port, get(&path, &second_auth)),
    );
    assert!(first.starts_with("HTTP/1.1 200"), "{first}");
    assert!(second.starts_with("HTTP/1.1 200"), "{second}");
    assert_eq!(tool.2.load(Ordering::SeqCst), attempts);
    assert_eq!(broker.resolves.load(Ordering::SeqCst), resolves);
    tool.1.store(false, Ordering::SeqCst);
    let before = ark
        .engine()
        .store
        .get_record(RECORD_KIND, &id)
        .unwrap()
        .unwrap();
    let mut due = before.clone();
    due["metadata_retry"]["next_at"] = json!("2000-01-01T00:00:00Z");
    ark.engine()
        .store
        .compare_record(RECORD_KIND, &id, &before, &due)
        .unwrap();
    let (first, second) = tokio::join!(
        raw(served.port, get(&path, &auth)),
        raw(served.port, get(&path, &second_auth)),
    );
    assert!(first.starts_with("HTTP/1.1 200"), "{first}");
    assert!(second.starts_with("HTTP/1.1 200"), "{second}");
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if ark.engine().connection_request(&id).unwrap().phase
                == ConnectionRequestPhase::Completed
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let final_request = ark.engine().connection_request(&id).unwrap();
    assert_eq!(final_request.invocation, original.invocation);
    assert!(final_request.metadata_retry.is_none());
    assert_eq!(tool.0.load(Ordering::SeqCst), 1);
    let turns = ark
        .engine()
        .store
        .list_session_turns(session.id().as_str(), None)
        .unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0]["input"], "Keep this exact request");
}

#[tokio::test]
async fn expired_or_replaced_metadata_request_closes_before_cooldown_or_upstream_work() {
    for expired_scope in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let (ark, _session, id, broker, tool) = fixture(dir.path()).await;
        tool.1.store(true, Ordering::SeqCst);
        broker.ready.store(true, Ordering::SeqCst);
        assert!(matches!(
            ark.engine().refresh_connection_request(&id).await,
            Err(ark_runtime::AgentError::Connection(
                ConnectionRequestError::MetadataUnavailable
            ))
        ));
        let before = ark
            .engine()
            .store
            .get_record(RECORD_KIND, &id)
            .unwrap()
            .unwrap();
        let mut changed = before.clone();
        changed["metadata_retry"]["next_at"] = json!("2030-01-01T00:00:00Z");
        if expired_scope {
            changed["scope"]["expiresAt"] = json!("2000-01-01T00:00:00Z");
        } else {
            changed["action_digest"] = json!("replaced-descriptor");
        }
        ark.engine()
            .store
            .compare_record(RECORD_KIND, &id, &before, &changed)
            .unwrap();
        let attempts = tool.2.load(Ordering::SeqCst);
        let resolves = broker.resolves.load(Ordering::SeqCst);
        let request = ark.engine().refresh_connection_request(&id).await.unwrap();
        assert!(
            matches!(request.phase, ConnectionRequestPhase::Closed { reason }
            if reason == if expired_scope { "expired" } else { "replaced" })
        );
        assert_eq!(tool.2.load(Ordering::SeqCst), attempts);
        assert_eq!(broker.resolves.load(Ordering::SeqCst), resolves);
    }
}

#[tokio::test]
async fn connection_http_broker_outage_keeps_original_chat_readable_without_resume() {
    let dir = tempfile::tempdir().unwrap();
    let (ark, session, id, broker, tool) = fixture(dir.path()).await;
    let (_, served, auth) = serve(&ark, dir.path()).await;
    broker.unavailable.store(true, Ordering::SeqCst);
    broker.ready.store(true, Ordering::SeqCst);
    let snapshot = snapshot_with_state(
        served.port,
        &format!("/client/v1/chats/{}", session.id()),
        &auth,
        "uncertain",
    )
    .await;
    assert_eq!(snapshot["connectionRequests"][0]["state"], "uncertain");
    assert!(snapshot["connectionRequests"][0]
        .get("scopeRef")
        .is_none_or(Value::is_null));
    assert_eq!(snapshot["messages"][0]["body"], "Keep this exact request");
    assert!(matches!(
        ark.engine().connection_request(&id).unwrap().phase,
        ConnectionRequestPhase::Waiting
    ));
    assert_eq!(tool.0.load(Ordering::SeqCst), 0);
    assert_eq!(
        ark.engine()
            .store
            .list_session_turns(session.id().as_str(), None)
            .unwrap()
            .len(),
        1
    );
}

fn baseline_sql_rows(
    root: &std::path::Path,
    mutable_session: &str,
) -> std::collections::BTreeMap<String, Vec<String>> {
    use rusqlite::types::ValueRef;
    let db = rusqlite::Connection::open_with_flags(
        root.join("meta.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let names: Vec<String> = db.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name").unwrap()
        .query_map([], |row| row.get(0)).unwrap().collect::<Result<_, _>>().unwrap();
    let mut snapshot = std::collections::BTreeMap::new();
    for name in names {
        let mut statement = db
            .prepare(&format!("SELECT * FROM \"{}\"", name.replace('"', "\"\"")))
            .unwrap();
        let columns = statement.column_count();
        let mut rows = statement.query([]).unwrap();
        let mut encoded = Vec::new();
        while let Some(row) = rows.next().unwrap() {
            if name == "sessions" && row.get::<_, String>(0).unwrap() == mutable_session {
                continue;
            }
            let values: Vec<Value> = (0..columns)
                .map(|column| match row.get_ref(column).unwrap() {
                    ValueRef::Null => json!(["null"]),
                    ValueRef::Integer(value) => json!(["integer", value.to_string()]),
                    ValueRef::Real(value) => json!(["real_bits", value.to_bits().to_string()]),
                    ValueRef::Text(value) => json!([
                        "text",
                        value
                            .iter()
                            .map(|byte| format!("{byte:02x}"))
                            .collect::<String>()
                    ]),
                    ValueRef::Blob(value) => json!([
                        "blob",
                        value
                            .iter()
                            .map(|byte| format!("{byte:02x}"))
                            .collect::<String>()
                    ]),
                })
                .collect();
            encoded.push(serde_json::to_string(&values).unwrap());
        }
        encoded.sort();
        snapshot.insert(name, encoded);
    }
    snapshot
}

fn credential_hashes(root: &std::path::Path) -> std::collections::BTreeMap<String, String> {
    ["access.json", "secrets.enc"]
        .into_iter()
        .filter_map(|name| {
            let path = root.join(name);
            path.is_file().then(|| {
                (
                    name.to_string(),
                    format!("{:x}", Sha256::digest(std::fs::read(path).unwrap())),
                )
            })
        })
        .collect()
}

fn snapshot_database(root: &std::path::Path, name: &str) {
    let destination = root.join(name);
    assert!(
        !destination.exists(),
        "fixture snapshot must not overwrite evidence"
    );
    rusqlite::Connection::open_with_flags(
        root.join("meta.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap()
    .execute("VACUUM INTO ?1", [destination.to_str().unwrap()])
    .unwrap();
}

#[tokio::test]
async fn rollback_stopped_unfinalized_boundary_keeps_only_legacy_persisted_shapes() {
    let temporary = tempfile::tempdir().unwrap();
    let external = std::env::var_os("ARK_STOPPED_ROLLBACK_ROOT").map(std::path::PathBuf::from);
    let root = external.as_deref().unwrap_or(temporary.path());
    assert!(
        !root.join("meta.db").exists(),
        "stopped fixture requires a fresh root"
    );
    let (ark, session, id, broker, _) = fixture(root).await;
    broker.ready.store(true, Ordering::SeqCst);
    let request = ark.engine().refresh_connection_request(&id).await.unwrap();
    let run = ark
        .engine()
        .admit_connection(&id, request.local_revision)
        .unwrap();
    ark.engine().start_connection(&run).unwrap();
    ark.control_session(session.id().as_str(), true).unwrap();
    let rows = ark
        .engine()
        .store
        .list_session_turns(session.id().as_str(), None)
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["connection"]["state"], "started");
    assert!(rows[0]["cancelled"].as_bool().unwrap());
    assert!(rows[0].get("pid").is_none());
    assert!(
        matches!(&ark.engine().connection_request(&id).unwrap().phase, ConnectionRequestPhase::Closed { reason } if reason == "cancelled")
    );
    let invocation = ark
        .engine()
        .store
        .get_invocation(&request.project, &request.invocation)
        .unwrap()
        .unwrap();
    assert_eq!(serde_json::to_value(&invocation).unwrap()["status"], "ok");
    assert!(invocation.effect.is_none());
    if external.is_some() {
        snapshot_database(root, "rollback-stopped-unfinalized.db");
    }
}

#[tokio::test]
async fn rollback_external_old_root_preserves_seed_and_exports_real_journal() {
    let temporary = tempfile::Builder::new()
        .prefix("ark-rollback-journal-")
        .tempdir()
        .unwrap();
    let external = std::env::var_os("ARK_ROLLBACK_FIXTURE_ROOT").map(std::path::PathBuf::from);
    let root = external.as_deref().unwrap_or(temporary.path());
    assert!(root.is_absolute());
    if external.is_some() {
        assert!(
            root.join("meta.db").is_file(),
            "external root must be an existing disposable R3 store"
        );
    }
    let phase = std::env::var("ARK_ROLLBACK_PHASE").unwrap_or_else(|_| "paused".into());
    assert!(matches!(phase.as_str(), "paused" | "claimed"));
    let external_session = external.as_ref().map(|_| {
        std::env::var("ARK_ROLLBACK_SESSION_ID")
            .expect("external fixture needs exact R3-created empty session ID")
    });
    let before_open_rows = external_session
        .as_ref()
        .map(|id| baseline_sql_rows(root, id));
    let before_open_header = external_session.as_ref().map(|id| {
        let db = rusqlite::Connection::open_with_flags(
            root.join("meta.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let json: String = db
            .query_row("SELECT json FROM sessions WHERE id=?1", [id], |row| {
                row.get(0)
            })
            .unwrap();
        serde_json::from_str::<Value>(&json).unwrap()
    });
    let before_credentials = credential_hashes(root);
    let broker = Arc::new(Broker::default());
    let tool = Arc::new(Connect::default());
    let ark = Ark::builder()
        .store_root(root)
        .tool(tool.clone())
        .connection_requests(broker.clone())
        .acquisition_source(Arc::new(Source))
        .profile(
            AgentProfile::builder()
                .core_actions(["fixture.connect"])
                .build(),
        )
        .provider(Arc::new(MockProvider::new(
            "fixture",
            vec![Completion {
                content: String::new(),
                tool_calls: vec![ToolCall {
                    id: "fixture-connect".into(),
                    name: "fixture.connect".into(),
                    arguments: json!({"offer":"sample"}),
                }],
                usage: Usage::default(),
            }],
        )))
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let session_id = if let Some(id) = external_session {
        id
    } else {
        let project = ark
            .find_or_create_project("Rollback fixture", root.to_str().unwrap())
            .unwrap();
        ark.start_session(&project.id).unwrap().id().to_string()
    };
    let session = ark.open_session(&session_id).unwrap().unwrap();
    assert_eq!(
        session.turn_count(),
        0,
        "seed chat must be empty; never overwrite a person request"
    );
    let project = session.project().clone();
    let baseline_sessions = ark.engine().store.list_sessions(None).unwrap();
    let baseline_projects = ark.engine().store.list_projects().unwrap();
    let baseline_calls = ark.engine().store.list_invocations(&project).unwrap();
    let baseline_rows = before_open_rows.unwrap_or_else(|| baseline_sql_rows(root, &session_id));
    let baseline_header =
        before_open_header.unwrap_or_else(|| serde_json::to_value(session.record()).unwrap());
    let start = ark_interact::Start {
        project: Some(project.to_string()),
        session: Some(session_id.clone()),
        ..Default::default()
    };
    let mut interaction = App::new("Rollback fixture")
        .ark(ark.clone())
        .open(&start, Arc::new(FixedApprover(ApprovalDecision::Deny)))
        .unwrap()
        .interaction;
    let mut journal = None;
    let end = interaction
        .run_turn(
            "Keep this exact rollback request",
            &ark_interact::TurnControl::new(),
            |event, _| {
                if matches!(event, HarnessEvent::AwaitingConnection { .. }) {
                    journal = Some(
                        ark.engine()
                            .store
                            .get_turn_trace(&session_id, 0)
                            .unwrap()
                            .unwrap(),
                    );
                    snapshot_database(root, "rollback-live-journal.db");
                }
            },
        )
        .await;
    let pending = end.result.unwrap().awaiting_connection.unwrap();
    let journal = journal.expect("real Journal must persist waiting before callback");
    assert!(journal["live"].as_bool().unwrap());
    assert!(journal["events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|timed| timed["event"]["type"] == "state_update"
            && timed["event"]["kind"] == "connection_waiting"));
    assert!(!journal.to_string().contains("awaiting_connection"));
    snapshot_database(root, "rollback-paused-final.db");
    if phase == "claimed" {
        broker.ready.store(true, Ordering::SeqCst);
        tool.1.store(true, Ordering::SeqCst);
        let attempted_after = time::OffsetDateTime::now_utc();
        assert!(matches!(
            ark.engine()
                .refresh_connection_request(&pending.request_id)
                .await,
            Err(ark_runtime::AgentError::Connection(
                ConnectionRequestError::MetadataUnavailable
            ))
        ));
        let claimed = ark
            .engine()
            .connection_request(&pending.request_id)
            .unwrap();
        assert_eq!(claimed.metadata_retry.as_ref().unwrap().attempts, 1);
        assert!(claimed.metadata_retry.as_ref().unwrap().next_at > attempted_after);
        assert!(matches!(
            ark.engine()
                .connection_request(&pending.request_id)
                .unwrap()
                .phase,
            ConnectionRequestPhase::Claimed { .. }
        ));
        snapshot_database(root, "rollback-claimed.db");
    }
    assert_eq!(
        ark.engine().store.list_sessions(None).unwrap().len(),
        baseline_sessions.len()
    );
    assert_eq!(
        ark.engine().store.list_projects().unwrap().len(),
        baseline_projects.len()
    );
    assert_eq!(
        ark.engine().store.list_invocations(&project).unwrap().len(),
        baseline_calls.len() + 1
    );
    assert_eq!(tool.0.load(Ordering::SeqCst), 0);
    let after_rows = baseline_sql_rows(root, &session_id);
    for (table, rows) in &baseline_rows {
        let after = after_rows.get(table).expect("baseline table disappeared");
        for row in rows {
            assert!(
                after.binary_search(row).is_ok(),
                "baseline row changed in {table}"
            );
        }
    }
    let after_header = ark
        .engine()
        .store
        .get_session(&session_id)
        .unwrap()
        .unwrap();
    for (key, value) in baseline_header.as_object().unwrap() {
        if ![
            "title",
            "updated_at",
            "turn_count",
            "usage",
            "posts_read",
            "gists",
            "dropped_turns",
        ]
        .contains(&key.as_str())
        {
            assert_eq!(
                after_header.get(key),
                Some(value),
                "baseline session field changed: {key}"
            );
        }
    }
    assert_eq!(credential_hashes(root), before_credentials);
    let exported_request = ark
        .engine()
        .connection_request(&pending.request_id)
        .unwrap();
    let exported_invocation = ark
        .engine()
        .store
        .get_invocation(&exported_request.project, &exported_request.invocation)
        .unwrap()
        .unwrap();
    assert_eq!(exported_invocation.id.as_str(), exported_request.invocation);
    assert_eq!(
        exported_invocation.connection_request.as_deref(),
        Some(exported_request.id.as_str())
    );
    assert_eq!(
        exported_invocation.work.as_deref(),
        Some(exported_request.work.as_str())
    );
    assert_eq!(
        exported_invocation.status,
        ark_core::InvocationStatus::PendingApproval
    );
    assert!(exported_invocation.finished_at.is_none());
    assert!(exported_invocation.effect.is_none());
    assert!(exported_invocation.output.is_null());
    let encoded_invocation = serde_json::to_value(&exported_invocation).unwrap();
    assert_eq!(encoded_invocation["status"], "pending_approval");
    assert!(encoded_invocation["finished_at"].is_null());
    let proof = json!({"externalOldSeed":external.is_some(),"session":session_id,"mutableSessionRow":"one original request appended; immutable fields compared",
        "baselineRowsSha256":format!("{:x}",Sha256::digest(serde_json::to_vec(&baseline_rows).unwrap())),
        "baselineRows":baseline_rows,
        "baselineRowsPreserved":true,"credentialHashes":before_credentials,
        "snapshots":["rollback-live-journal.db","rollback-paused-final.db"],"claimedSnapshot":phase == "claimed"});
    std::fs::write(
        root.join("rollback-baseline-proof.json"),
        serde_json::to_vec_pretty(&proof).unwrap(),
    )
    .unwrap();
    let final_trace = ark
        .engine()
        .store
        .get_turn_trace(&session_id, 0)
        .unwrap()
        .unwrap();
    assert!(!final_trace.to_string().contains("awaiting_connection"));
    assert!(final_trace["events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|timed| timed["event"]["type"] == "state_update"
            && timed["event"]["kind"] == "connection_waiting"));
    drop(interaction);
    drop(session);
    drop(ark);
    if external.is_some() || std::env::var_os("ARK_KEEP_ROLLBACK_FIXTURES").is_some() {
        let retained = match external {
            Some(root) => root,
            None => temporary.keep(),
        };
        let file_proof = |file: &str, boundary: &str| {
            let bytes = std::fs::read(retained.join(file)).unwrap();
            json!({"file":file,"boundary":boundary,
                "sha256":format!("{:x}",Sha256::digest(&bytes)),"sizeBytes":bytes.len()})
        };
        let mut snapshots = vec![
            file_proof(
                "rollback-live-journal.db",
                "live_journal_before_request_commit",
            ),
            file_proof("rollback-paused-final.db", "counted_waiting_before_claim"),
        ];
        if phase == "claimed" {
            snapshots.push(file_proof(
                "rollback-claimed.db",
                "claimed_metadata_retry_before_completion",
            ));
        }
        let executable = std::env::current_exe().unwrap();
        let executable_bytes = std::fs::read(&executable).unwrap();
        let request_phase = match &exported_request.phase {
            ConnectionRequestPhase::Preparing => "preparing",
            ConnectionRequestPhase::Claimed { .. } => "claimed",
            _ => panic!("unexpected fixture request phase"),
        };
        let manifest = json!({
            "schemaVersion":1,"fixtureOnly":true,"externalOldSeed":proof["externalOldSeed"],
            "session":session_id,"project":exported_request.project,
            "request":exported_request.id,"invocation":exported_request.invocation,
            "work":exported_request.work,"casePhase":phase,
            "requestPhase":request_phase,
            "invocationStatus":encoded_invocation["status"],"finishedAt":encoded_invocation["finished_at"],
            "effect":null,"outputIsNull":true,"metadataRetry":exported_request.metadata_retry,
            "selectedSnapshot":if phase == "claimed" { "rollback-claimed.db" } else { "rollback-paused-final.db" },
            "snapshots":snapshots,"baselineProof":file_proof("rollback-baseline-proof.json", "baseline_rows_and_credentials"),
            "credentialHashes":proof["credentialHashes"],
            "serializerProof":{"liveJournalCaptured":true,"liveAndFinalWaitingEvent":"state_update","awaitingConnectionAbsent":true},
            "producer":{"testName":"rollback_external_old_root_preserves_seed_and_exports_real_journal",
                "packageVersion":env!("CARGO_PKG_VERSION"),"currentTestBinaryPath":executable.display().to_string(),
                "currentTestBinarySha256":format!("{:x}",Sha256::digest(&executable_bytes)),
                "currentTestBinarySize":executable_bytes.len()},
            "providerNetworkCalls":0,
            "networkEvidence":"fixture construction only; not a production transport observation",
            "providerKind":"MockProvider","brokerKind":"in_memory_fixture","adapterKind":"metadata_fixture_no_network"
        });
        std::fs::write(
            retained.join("rollback-export-manifest.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        println!("ROLLBACK_FIXTURE phase={phase} root={} session={session_id} request={} realJournal=true externalOldSeed={}",retained.display(),pending.request_id,std::env::var_os("ARK_ROLLBACK_FIXTURE_ROOT").is_some());
    }
}
