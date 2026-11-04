use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use ark_core::{ActionDescriptor, Invocation, ProjectId};
use ark_tools::ClaimedConnectionBinding;
use serde::{Deserialize, Serialize};

pub const RECORD_KIND: &str = "chat_connection_request";
pub use ark_store::RecordChange;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextualAcquisition {
    pub offer_id: String,
    pub display: String,
}

#[derive(Clone, Debug, thiserror::Error)]
pub enum ConnectionRequestError {
    #[error("connection request refused: {0}")]
    Refused(String),
    #[error("the connection request changed; refresh it")]
    Stale,
    #[error("connection service unavailable: {0}")]
    Unavailable(String),
    #[error("connection status could not be confirmed")]
    Uncertain,
    #[error("connection metadata is temporarily unavailable")]
    MetadataUnavailable,
    #[error("the conversation is starting; try again")]
    Busy,
}

pub type ConnectionResult<T> = Result<T, ConnectionRequestError>;

async fn connection_io<T>(
    future: impl std::future::Future<Output = ConnectionResult<T>>,
) -> ConnectionResult<T> {
    tokio::time::timeout(std::time::Duration::from_secs(10), future)
        .await
        .map_err(|_| ConnectionRequestError::Unavailable("connection request timed out".into()))?
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionScopeRequest {
    pub request_id: String,
    pub invocation_id: String,
    pub offer_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparedConnectionScope {
    pub scope_ref: String,
    #[serde(with = "ark_tools::decimal_revision")]
    pub broker_revision: u64,
    pub expires_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveConnectionScope {
    pub scope_ref: String,
    pub request_id: String,
    pub invocation_id: String,
    pub route_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionScopeState {
    Pending,
    Ready,
    Claimed,
    Cancelled,
    Expired,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedConnectionScope {
    pub scope_ref: String,
    pub request_id: String,
    pub invocation_id: String,
    pub route_id: String,
    pub offer_id: String,
    pub fingerprint: String,
    #[serde(with = "ark_tools::decimal_revision")]
    pub broker_revision: u64,
    pub state: ConnectionScopeState,
    pub claim_receipt: Option<ClaimedConnectionBinding>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaimConnectionScope {
    pub scope_ref: String,
    pub request_id: String,
    pub invocation_id: String,
    #[serde(with = "ark_tools::decimal_revision")]
    pub expected_broker_revision: u64,
}

#[async_trait::async_trait]
pub trait ConnectionRequestBroker: Send + Sync {
    fn route_id(&self) -> &str;
    async fn prepare(
        &self,
        request: ConnectionScopeRequest,
    ) -> ConnectionResult<PreparedConnectionScope>;
    async fn resolve(
        &self,
        request: ResolveConnectionScope,
    ) -> ConnectionResult<ResolvedConnectionScope>;
    async fn claim(
        &self,
        request: ClaimConnectionScope,
    ) -> ConnectionResult<ClaimedConnectionBinding>;
    async fn cancel(
        &self,
        request: ClaimConnectionScope,
    ) -> ConnectionResult<ResolvedConnectionScope>;
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AwaitingConnection {
    pub request_id: String,
    pub call_id: String,
    pub invocation: Invocation,
    pub requirement: ContextualAcquisition,
    pub action_digest: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PendingConnectionRequest {
    pub id: String,
    pub project: ProjectId,
    pub session: String,
    pub turn: u32,
    pub work: String,
    pub invocation: String,
    #[serde(default)]
    pub call_id: String,
    pub action_digest: String,
    pub requirement: ContextualAcquisition,
    pub local_revision: u64,
    pub scope: Option<PreparedConnectionScope>,
    pub resolution: Option<ResolvedConnectionScope>,
    pub phase: ConnectionRequestPhase,
    #[serde(default)]
    pub cancel_pending: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata_retry: Option<ConnectionMetadataRetry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionMetadataRetry {
    pub attempts: u32,
    #[serde(with = "time::serde::rfc3339")]
    pub next_at: time::OffsetDateTime,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ConnectionRequestPhase {
    Preparing,
    Waiting,
    Claimed {
        binding: ClaimedConnectionBinding,
    },
    Acquired {
        binding: ClaimedConnectionBinding,
    },
    Admitted {
        run_id: String,
    },
    Started {
        run_id: String,
        pid: u32,
        pid_start: Option<u64>,
    },
    Completed,
    Interrupted {
        run_id: String,
    },
    Closed {
        reason: String,
    },
}

impl PendingConnectionRequest {
    pub fn closed(&self) -> bool {
        matches!(
            self.phase,
            ConnectionRequestPhase::Completed
                | ConnectionRequestPhase::Interrupted { .. }
                | ConnectionRequestPhase::Closed { .. }
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum TurnConnection {
    Waiting {
        request_id: String,
    },
    Admitted {
        request_id: String,
        run_id: String,
    },
    Started {
        request_id: String,
        run_id: String,
        pid: u32,
        pid_start: Option<u64>,
    },
}

#[derive(Clone, Debug)]
pub struct AdmittedConnectionRun {
    pub(crate) request_id: String,
    pub(crate) run_id: String,
    pub(crate) turn: u32,
}

impl AdmittedConnectionRun {
    pub fn request_id(&self) -> &str {
        &self.request_id
    }
    pub fn turn(&self) -> u32 {
        self.turn
    }
}

#[derive(Default)]
pub(crate) struct ChatAdmission {
    chats: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    requests: Mutex<HashMap<String, std::sync::Weak<tokio::sync::Mutex<()>>>>,
}

impl ChatAdmission {
    fn request(&self, id: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut requests = self.requests.lock().unwrap();
        requests.retain(|_, flight| flight.strong_count() > 0);
        if let Some(flight) = requests.get(id).and_then(std::sync::Weak::upgrade) {
            return flight;
        }
        let flight = Arc::new(tokio::sync::Mutex::new(()));
        requests.insert(id.into(), Arc::downgrade(&flight));
        flight
    }

    pub(crate) fn chat(&self, session: &str) -> Arc<Mutex<()>> {
        self.chats
            .lock()
            .unwrap()
            .entry(session.into())
            .or_default()
            .clone()
    }
}

pub(crate) fn descriptor_digest(
    action: &ActionDescriptor,
    input: &serde_json::Value,
) -> ConnectionResult<String> {
    if !action.contextual_acquisition
        || action.risk != ark_core::RiskTier::Observe
        || action.reuse != ark_core::Reuse::Volatile
    {
        return Err(ConnectionRequestError::Refused(
            "acquisition requires an Observe, volatile descriptor".into(),
        ));
    }
    let value = serde_json::json!({ "descriptor": action, "input": input });
    Ok(ark_core::ArtifactId::of_bytes(value.to_string().as_bytes()).to_string())
}

impl crate::Engine {
    pub fn set_connection_requests(&mut self, broker: Arc<dyn ConnectionRequestBroker>) {
        self.connection_broker = Some(broker);
    }

    pub fn has_connection_requests(&self) -> bool {
        self.connection_broker.is_some() && self.connection_source.is_some()
    }

    pub fn set_contextual_acquisition(&mut self, source: Arc<dyn crate::AcquisitionProvider>) {
        self.connection_source = Some(source);
    }

    pub fn connection_requests(
        &self,
        session: &str,
    ) -> Result<Vec<PendingConnectionRequest>, crate::AgentError> {
        self.store
            .list_records(RECORD_KIND, Some(session))?
            .into_iter()
            .map(|value| serde_json::from_value(value).map_err(Into::into))
            .collect()
    }

    pub fn connection_request(
        &self,
        id: &str,
    ) -> Result<PendingConnectionRequest, crate::AgentError> {
        let value = self
            .store
            .get_record(RECORD_KIND, id)?
            .ok_or(ConnectionRequestError::Stale)?;
        Ok(serde_json::from_value(value)?)
    }

    fn connection_request_bound(
        &self,
        request: &PendingConnectionRequest,
    ) -> Result<bool, crate::AgentError> {
        let Some(header) = self.store.session_version(&request.session)? else {
            return Ok(false);
        };
        let header: serde_json::Value = serde_json::from_str(&header)?;
        if header["turn_count"].as_u64() != Some(u64::from(request.turn) + 1) {
            return Ok(false);
        }
        let Some(value) = self
            .store
            .list_session_turns(&request.session, Some(1))?
            .into_iter()
            .next()
        else {
            return Ok(false);
        };
        let turn: crate::session::Turn = serde_json::from_value(value)?;
        let marker_matches = match (&turn.connection, &request.phase) {
            (
                Some(TurnConnection::Waiting { request_id }),
                ConnectionRequestPhase::Preparing
                | ConnectionRequestPhase::Waiting
                | ConnectionRequestPhase::Claimed { .. }
                | ConnectionRequestPhase::Acquired { .. },
            ) => request_id == &request.id,
            (
                Some(TurnConnection::Admitted { request_id, run_id }),
                ConnectionRequestPhase::Admitted { run_id: expected },
            ) => request_id == &request.id && run_id == expected,
            (
                Some(TurnConnection::Started {
                    request_id,
                    run_id,
                    pid,
                    pid_start,
                }),
                ConnectionRequestPhase::Started {
                    run_id: expected,
                    pid: expected_pid,
                    pid_start: expected_start,
                },
            ) => {
                request_id == &request.id
                    && run_id == expected
                    && pid == expected_pid
                    && pid_start == expected_start
            }
            _ => false,
        };
        let Some(invocation) = self
            .store
            .get_invocation(&request.project, &request.invocation)?
        else {
            return Ok(false);
        };
        let expected_status = if matches!(
            request.phase,
            ConnectionRequestPhase::Preparing
                | ConnectionRequestPhase::Waiting
                | ConnectionRequestPhase::Claimed { .. }
        ) {
            ark_core::InvocationStatus::PendingApproval
        } else {
            ark_core::InvocationStatus::Ok
        };
        Ok(marker_matches
            && turn.index == request.turn
            && invocation.connection_request.as_deref() == Some(request.id.as_str())
            && invocation.work.as_deref() == Some(request.work.as_str())
            && invocation.status == expected_status)
    }

    fn retire_connection_orphan(
        &self,
        request: &PendingConnectionRequest,
    ) -> Result<bool, crate::AgentError> {
        if request.closed() || self.connection_request_bound(request)? {
            return Ok(false);
        }
        let mut next = request.clone();
        next.phase = ConnectionRequestPhase::Closed {
            reason: "replaced".into(),
        };
        next.cancel_pending = request.scope.is_some();
        next.local_revision = next
            .local_revision
            .checked_add(1)
            .ok_or(ConnectionRequestError::Stale)?;
        self.store.compare_record(
            RECORD_KIND,
            &request.id,
            &serde_json::to_value(request)?,
            &serde_json::to_value(next)?,
        )?;
        Ok(true)
    }

    fn transition_connection(
        &self,
        before: &PendingConnectionRequest,
        after: PendingConnectionRequest,
        connection: Option<TurnConnection>,
        invocation: Option<&Invocation>,
    ) -> Result<(), crate::AgentError> {
        self.transition_connection_records(before, after, connection, invocation, &[])
    }

    fn transition_connection_records(
        &self,
        before: &PendingConnectionRequest,
        mut after: PendingConnectionRequest,
        connection: Option<TurnConnection>,
        invocation: Option<&Invocation>,
        extra: &[ark_store::RecordChange<'_>],
    ) -> Result<(), crate::AgentError> {
        let admission = self.chat_admission.chat(&before.session);
        let _guard = admission.lock().unwrap();
        if !self.connection_request_bound(before)? {
            self.retire_connection_orphan(before)?;
            return Err(ConnectionRequestError::Stale.into());
        }
        let header = self
            .store
            .session_version(&before.session)?
            .ok_or(ConnectionRequestError::Stale)?;
        let mut header_value: serde_json::Value = serde_json::from_str(&header)?;
        if header_value["turn_count"].as_u64() != Some(before.turn as u64 + 1) {
            return Err(ConnectionRequestError::Stale.into());
        }
        let turn_value = self
            .store
            .list_session_turns(&before.session, Some(1))?
            .into_iter()
            .next()
            .ok_or(ConnectionRequestError::Stale)?;
        let mut turn: crate::session::Turn = serde_json::from_value(turn_value.clone())?;
        let owner = match &turn.connection {
            Some(TurnConnection::Waiting { request_id })
            | Some(TurnConnection::Admitted { request_id, .. })
            | Some(TurnConnection::Started { request_id, .. }) => request_id,
            None => return Err(ConnectionRequestError::Stale.into()),
        };
        if owner != &before.id || turn.index != before.turn {
            return Err(ConnectionRequestError::Stale.into());
        }
        turn.connection = connection;
        if let Some(TurnConnection::Started { pid, pid_start, .. }) = &turn.connection {
            turn.pid = Some(*pid);
            turn.pid_start = *pid_start;
        }
        if invocation.is_some() && matches!(after.phase, ConnectionRequestPhase::Acquired { .. }) {
            turn.metrics.record_tool_call();
            header_value["usage"]["tool_calls"] = serde_json::Value::from(
                header_value["usage"]["tool_calls"].as_u64().unwrap_or(0) + 1,
            );
        }
        if matches!(after.phase, ConnectionRequestPhase::Closed { .. }) {
            turn.cancelled = true;
            turn.pid = None;
            turn.pid_start = None;
        }
        if matches!(after.phase, ConnectionRequestPhase::Interrupted { .. }) {
            turn.incomplete = true;
            turn.error = Some(crate::session::INTERRUPTED.into());
            turn.pid = None;
            turn.pid_start = None;
        }
        after.local_revision = before
            .local_revision
            .checked_add(1)
            .ok_or(ConnectionRequestError::Stale)?;
        let old = serde_json::to_value(before)?;
        let next = serde_json::to_value(&after)?;
        let mut records = vec![ark_store::RecordChange {
            kind: RECORD_KIND,
            id: &before.id,
            scope: &before.session,
            expected: Some(&old),
            value: &next,
        }];
        records.extend(extra.iter().map(|record| ark_store::RecordChange {
            kind: record.kind,
            id: record.id,
            scope: record.scope,
            expected: record.expected,
            value: record.value,
        }));
        self.store.transition_chat(ark_store::ChatTransition {
            session: &before.session,
            expected_header: &header,
            header: &header_value,
            index: before.turn,
            expected_turn: Some(&turn_value),
            turn: &serde_json::to_value(turn)?,
            records: &records,
            invocation: invocation.map(|invocation| (&before.project, invocation)),
            other_turn: None,
        })?;
        Ok(())
    }

    pub async fn refresh_connection_request(
        &self,
        id: &str,
    ) -> Result<PendingConnectionRequest, crate::AgentError> {
        let flight = self.chat_admission.request(id);
        let _guard = flight.lock().await;
        let result = self.refresh_connection_request_locked(id).await;
        if matches!(
            &result,
            Err(crate::AgentError::Connection(
                ConnectionRequestError::Refused(_)
            ))
        ) {
            let request = self.connection_request(id)?;
            if !request.closed() {
                let mut closed = request.clone();
                closed.phase = ConnectionRequestPhase::Closed {
                    reason: "unavailable".into(),
                };
                closed.cancel_pending = request.scope.is_some();
                self.transition_connection(&request, closed, None, None)?;
                return self.connection_request(id);
            }
        }
        result
    }

    async fn refresh_connection_request_locked(
        &self,
        id: &str,
    ) -> Result<PendingConnectionRequest, crate::AgentError> {
        let broker = self
            .connection_broker
            .as_ref()
            .ok_or_else(|| ConnectionRequestError::Unavailable("no connection broker".into()))?;
        let mut request = self.connection_request(id)?;
        if self.retire_connection_orphan(&request)? {
            return self.connection_request(id);
        }
        if request.closed()
            || matches!(
                request.phase,
                ConnectionRequestPhase::Admitted { .. }
                    | ConnectionRequestPhase::Started { .. }
                    | ConnectionRequestPhase::Acquired { .. }
            )
        {
            if request.cancel_pending {
                self.flush_connection_cancel(&request).await?;
                return self.connection_request(id);
            }
            return Ok(request);
        }
        if let Some(scope) = &request.scope {
            let expiry = time::OffsetDateTime::parse(
                &scope.expires_at,
                &time::format_description::well_known::Rfc3339,
            )
            .map_err(|_| ConnectionRequestError::Uncertain)?;
            if expiry <= time::OffsetDateTime::now_utc() {
                let mut closed = request.clone();
                closed.phase = ConnectionRequestPhase::Closed {
                    reason: "expired".into(),
                };
                closed.cancel_pending = true;
                self.transition_connection(&request, closed, None, None)?;
                return self.connection_request(id);
            }
        }
        if let ConnectionRequestPhase::Claimed { binding } = &request.phase {
            let invocation = self
                .store
                .get_invocation(&request.project, &request.invocation)?
                .ok_or(ConnectionRequestError::Stale)?;
            if !self.connection_metadata_matches(&request, binding, &invocation)? {
                let mut replaced = request.clone();
                replaced.phase = ConnectionRequestPhase::Closed {
                    reason: "replaced".into(),
                };
                replaced.cancel_pending = request.scope.is_some();
                self.transition_connection(&request, replaced, None, None)?;
                return self.connection_request(id);
            }
            if request
                .metadata_retry
                .as_ref()
                .is_some_and(|retry| retry.next_at > time::OffsetDateTime::now_utc())
            {
                return Err(ConnectionRequestError::MetadataUnavailable.into());
            }
        }
        if request.scope.is_none() {
            let scope = connection_io(broker.prepare(ConnectionScopeRequest {
                request_id: request.id.clone(),
                invocation_id: request.invocation.clone(),
                offer_id: request.requirement.offer_id.clone(),
            }))
            .await?;
            let expiry = time::OffsetDateTime::parse(
                &scope.expires_at,
                &time::format_description::well_known::Rfc3339,
            )
            .map_err(|_| ConnectionRequestError::Uncertain)?;
            if scope.scope_ref.is_empty() || expiry <= time::OffsetDateTime::now_utc() {
                return Err(ConnectionRequestError::Uncertain.into());
            }
            let mut next = request.clone();
            next.scope = Some(scope);
            next.phase = ConnectionRequestPhase::Waiting;
            self.transition_connection(
                &request,
                next,
                Some(TurnConnection::Waiting {
                    request_id: request.id.clone(),
                }),
                None,
            )?;
            request = self.connection_request(id)?;
        }
        let scope = request
            .scope
            .as_ref()
            .ok_or(ConnectionRequestError::Uncertain)?;
        let resolved = connection_io(broker.resolve(ResolveConnectionScope {
            scope_ref: scope.scope_ref.clone(),
            request_id: request.id.clone(),
            invocation_id: request.invocation.clone(),
            route_id: broker.route_id().into(),
        }))
        .await?;
        if resolved.scope_ref != scope.scope_ref
            || resolved.request_id != request.id
            || resolved.invocation_id != request.invocation
            || resolved.route_id != broker.route_id()
            || resolved.offer_id != request.requirement.offer_id
            || resolved.fingerprint.is_empty()
            || resolved.broker_revision < scope.broker_revision
        {
            return Err(ConnectionRequestError::Uncertain.into());
        }
        let replaced = request
            .resolution
            .as_ref()
            .is_some_and(|old| old.fingerprint != resolved.fingerprint);
        if request.resolution.as_ref() != Some(&resolved) {
            let mut next = request.clone();
            next.resolution = Some(resolved.clone());
            if replaced
                || matches!(
                    resolved.state,
                    ConnectionScopeState::Cancelled
                        | ConnectionScopeState::Expired
                        | ConnectionScopeState::Unavailable
                )
            {
                next.phase = ConnectionRequestPhase::Closed {
                    reason: if replaced { "replaced" } else { "unavailable" }.into(),
                };
                self.transition_connection(&request, next, None, None)?;
                return self.connection_request(id);
            }
            self.transition_connection(
                &request,
                next,
                Some(TurnConnection::Waiting {
                    request_id: request.id.clone(),
                }),
                None,
            )?;
            request = self.connection_request(id)?;
        }
        if matches!(
            resolved.state,
            ConnectionScopeState::Ready | ConnectionScopeState::Claimed
        ) {
            let binding = match resolved.state {
                ConnectionScopeState::Claimed => resolved
                    .claim_receipt
                    .clone()
                    .ok_or(ConnectionRequestError::Uncertain)?,
                ConnectionScopeState::Ready
                    if !matches!(request.phase, ConnectionRequestPhase::Claimed { .. }) =>
                {
                    connection_io(broker.claim(ClaimConnectionScope {
                        scope_ref: resolved.scope_ref.clone(),
                        request_id: request.id.clone(),
                        invocation_id: request.invocation.clone(),
                        expected_broker_revision: resolved.broker_revision,
                    }))
                    .await?
                }
                _ => return Err(ConnectionRequestError::Uncertain.into()),
            };
            if binding.scope_ref != resolved.scope_ref
                || binding.request_id != request.id
                || binding.invocation_id != request.invocation
                || binding.route_id != resolved.route_id
                || binding.offer_id != resolved.offer_id
                || binding.fingerprint != resolved.fingerprint
                || binding.binding_id.is_empty()
                || binding.receipt_id.is_empty()
                || binding.broker_revision < resolved.broker_revision
                || (resolved.state == ConnectionScopeState::Claimed
                    && binding.broker_revision != resolved.broker_revision)
                || matches!(&request.phase, ConnectionRequestPhase::Claimed { binding: prior } if prior != &binding)
            {
                return Err(ConnectionRequestError::Uncertain.into());
            }
            if !matches!(request.phase, ConnectionRequestPhase::Claimed { .. }) {
                let mut next = request.clone();
                next.phase = ConnectionRequestPhase::Claimed { binding };
                self.transition_connection(
                    &request,
                    next,
                    Some(TurnConnection::Waiting {
                        request_id: request.id.clone(),
                    }),
                    None,
                )?;
            }
            self.complete_connection_acquisition(id).await?;
        }
        self.connection_request(id)
    }

    fn connection_metadata_matches(
        &self,
        request: &PendingConnectionRequest,
        binding: &ClaimedConnectionBinding,
        invocation: &Invocation,
    ) -> Result<bool, crate::AgentError> {
        let registry = self.tools();
        let descriptor = registry.descriptor(&invocation.action);
        let matches_descriptor = match &descriptor {
            Some(descriptor) => match descriptor_digest(descriptor, &invocation.input) {
                Ok(digest) => digest == request.action_digest,
                Err(ConnectionRequestError::Refused(_)) => false,
                Err(error) => return Err(error.into()),
            },
            None => false,
        };
        Ok(matches_descriptor
            && invocation.connection_request.as_deref() == Some(request.id.as_str())
            && invocation.status == ark_core::InvocationStatus::PendingApproval
            && invocation.work.as_deref() == Some(&request.work)
            && binding.offer_id == request.requirement.offer_id
            && binding.invocation_id == invocation.id.as_str())
    }

    async fn complete_connection_acquisition(&self, id: &str) -> Result<(), crate::AgentError> {
        let mut request = self.connection_request(id)?;
        let ConnectionRequestPhase::Claimed { binding } = &request.phase else {
            return Err(ConnectionRequestError::Stale.into());
        };
        let binding = binding.clone();
        let mut invocation = self
            .store
            .get_invocation(&request.project, &request.invocation)?
            .ok_or(ConnectionRequestError::Stale)?;
        if !self.connection_metadata_matches(&request, &binding, &invocation)? {
            let mut replaced = request.clone();
            replaced.phase = ConnectionRequestPhase::Closed {
                reason: "replaced".into(),
            };
            replaced.cancel_pending = request.scope.is_some();
            self.transition_connection(&request, replaced, None, None)?;
            return Err(ConnectionRequestError::Stale.into());
        }
        let now = time::OffsetDateTime::now_utc();
        if request
            .metadata_retry
            .as_ref()
            .is_some_and(|retry| retry.next_at > now)
        {
            return Err(ConnectionRequestError::MetadataUnavailable.into());
        }
        let attempts = request
            .metadata_retry
            .as_ref()
            .map_or(1, |retry| retry.attempts.saturating_add(1));
        let delay = (1_i64 << attempts.saturating_sub(1).min(5)).min(30);
        let mut reserved = request.clone();
        reserved.metadata_retry = Some(ConnectionMetadataRetry {
            attempts,
            next_at: now
                .checked_add(time::Duration::seconds(delay))
                .ok_or(ConnectionRequestError::Uncertain)?,
        });
        let expected_retry = reserved.metadata_retry.clone();
        let expected_revision = request
            .local_revision
            .checked_add(1)
            .ok_or(ConnectionRequestError::Stale)?;
        self.transition_connection(
            &request,
            reserved,
            Some(TurnConnection::Waiting {
                request_id: request.id.clone(),
            }),
            None,
        )?;
        request = self.connection_request(id)?;
        if request.local_revision != expected_revision
            || request.metadata_retry != expected_retry
            || !matches!(&request.phase, ConnectionRequestPhase::Claimed { binding: current } if current == &binding)
        {
            return Err(ConnectionRequestError::Stale.into());
        }
        let blobs = crate::blobs::StoreBlobs(&self.store);
        let mut ctx = ark_tools::RunCtx::new(&blobs)
            .with_workspace(self.workspace_dir(&request.project)?)
            .with_work(Some(request.work.clone()), Vec::new())
            .with_invocation(&request.invocation);
        ctx.connection_binding = Some(binding.clone());
        let registry = self.tools();
        let output = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            registry.run(&invocation.action, invocation.input.clone(), &ctx),
        )
        .await
        .map_err(|_| ConnectionRequestError::MetadataUnavailable)?
        .map_err(|_| ConnectionRequestError::MetadataUnavailable)?;
        if output.effect.is_some()
            || !output.produced.is_empty()
            || output.command.is_some()
            || !output.pictures.is_empty()
        {
            invocation.effect = output.effect;
            invocation.mark_failed("acquisition returned more than connection metadata");
            let mut closed = request.clone();
            closed.phase = ConnectionRequestPhase::Closed {
                reason: "invalid_acquisition_result".into(),
            };
            closed.cancel_pending = request.scope.is_some();
            self.transition_connection(&request, closed, None, Some(&invocation))?;
            return Err(ConnectionRequestError::Refused(
                "acquisition must return metadata only".into(),
            )
            .into());
        }
        invocation.mark_ok(output.output);
        let mut next = request.clone();
        next.phase = ConnectionRequestPhase::Acquired {
            binding: binding.clone(),
        };
        next.metadata_retry = None;
        self.transition_connection(
            &request,
            next,
            Some(TurnConnection::Waiting {
                request_id: request.id.clone(),
            }),
            Some(&invocation),
        )?;
        Ok(())
    }

    pub fn admit_connection(
        &self,
        id: &str,
        expected_revision: u64,
    ) -> Result<AdmittedConnectionRun, crate::AgentError> {
        self.admit_connection_record(id, expected_revision, None)
    }

    pub fn admit_connection_record(
        &self,
        id: &str,
        expected_revision: u64,
        receipt: Option<ark_store::RecordChange<'_>>,
    ) -> Result<AdmittedConnectionRun, crate::AgentError> {
        let request = self.connection_request(id)?;
        if let ConnectionRequestPhase::Admitted { run_id }
        | ConnectionRequestPhase::Started { run_id, .. } = &request.phase
        {
            if let Some(receipt) = receipt {
                let connection = match &request.phase {
                    ConnectionRequestPhase::Started { pid, pid_start, .. } => {
                        TurnConnection::Started {
                            request_id: request.id.clone(),
                            run_id: run_id.clone(),
                            pid: *pid,
                            pid_start: *pid_start,
                        }
                    }
                    _ => TurnConnection::Admitted {
                        request_id: request.id.clone(),
                        run_id: run_id.clone(),
                    },
                };
                self.transition_connection_records(
                    &request,
                    request.clone(),
                    Some(connection),
                    None,
                    &[receipt],
                )?;
            }
            return Ok(AdmittedConnectionRun {
                request_id: request.id.clone(),
                run_id: run_id.clone(),
                turn: request.turn,
            });
        }
        if request.local_revision != expected_revision
            || !matches!(request.phase, ConnectionRequestPhase::Acquired { .. })
        {
            return Err(ConnectionRequestError::Stale.into());
        }
        let run_id = ark_core::InvocationId::new().to_string();
        let mut next = request.clone();
        next.phase = ConnectionRequestPhase::Admitted {
            run_id: run_id.clone(),
        };
        let receipts: Vec<_> = receipt.into_iter().collect();
        self.transition_connection_records(
            &request,
            next,
            Some(TurnConnection::Admitted {
                request_id: request.id.clone(),
                run_id: run_id.clone(),
            }),
            None,
            &receipts,
        )?;
        Ok(AdmittedConnectionRun {
            request_id: request.id,
            run_id,
            turn: request.turn,
        })
    }

    pub fn start_connection(
        &self,
        run: &AdmittedConnectionRun,
    ) -> Result<crate::session::Turn, crate::AgentError> {
        let request = self.connection_request(&run.request_id)?;
        if request.phase
            != (ConnectionRequestPhase::Admitted {
                run_id: run.run_id.clone(),
            })
            || request.turn != run.turn
        {
            return Err(ConnectionRequestError::Stale.into());
        }
        let pid = std::process::id();
        let pid_start = crate::process::own_start();
        let mut next = request.clone();
        next.phase = ConnectionRequestPhase::Started {
            run_id: run.run_id.clone(),
            pid,
            pid_start,
        };
        self.transition_connection(
            &request,
            next,
            Some(TurnConnection::Started {
                request_id: request.id.clone(),
                run_id: run.run_id.clone(),
                pid,
                pid_start,
            }),
            None,
        )?;
        let result: Result<crate::session::Turn, crate::AgentError> = (|| {
            let value = self
                .store
                .list_session_turns(&request.session, Some(1))?
                .into_iter()
                .next()
                .ok_or(ConnectionRequestError::Stale)?;
            Ok(serde_json::from_value(value)?)
        })();
        if let Err(error) = &result {
            self.interrupt_connection(run).map_err(|recovery| {
                ConnectionRequestError::Unavailable(format!(
                    "continuation setup failed: {error}; interruption failed: {recovery}"
                ))
            })?;
        }
        result
    }

    pub fn interrupt_connection(
        &self,
        run: &AdmittedConnectionRun,
    ) -> Result<(), crate::AgentError> {
        let request = self.connection_request(&run.request_id)?;
        if !matches!(&request.phase, ConnectionRequestPhase::Started { run_id, .. } if run_id == &run.run_id)
        {
            return Ok(());
        }
        let mut next = request.clone();
        next.phase = ConnectionRequestPhase::Interrupted {
            run_id: run.run_id.clone(),
        };
        match self.transition_connection(&request, next, None, None) {
            Err(
                error @ (crate::AgentError::Connection(ConnectionRequestError::Stale)
                | crate::AgentError::Store(ark_store::StoreError::Conflict)),
            ) => {
                let current = self.connection_request(&run.request_id)?;
                if matches!(&current.phase, ConnectionRequestPhase::Started { run_id, .. } if run_id == &run.run_id)
                {
                    Err(error)
                } else {
                    Ok(())
                }
            }
            result => result,
        }
    }

    pub fn connection_control(&self, session: &str, stop: bool) -> Result<bool, crate::AgentError> {
        let active = self
            .connection_requests(session)?
            .into_iter()
            .find(|request| !request.closed());
        let Some(request) = active else {
            return Ok(false);
        };
        if !stop {
            if matches!(request.phase, ConnectionRequestPhase::Admitted { .. }) {
                return Err(ConnectionRequestError::Busy.into());
            }
            return Ok(matches!(
                request.phase,
                ConnectionRequestPhase::Started { .. }
            ));
        }
        let mut next = request.clone();
        next.phase = ConnectionRequestPhase::Closed {
            reason: "cancelled".into(),
        };
        next.cancel_pending = request.scope.is_some();
        let marker = match &request.phase {
            ConnectionRequestPhase::Started {
                run_id,
                pid,
                pid_start,
            } => Some(TurnConnection::Started {
                request_id: request.id.clone(),
                run_id: run_id.clone(),
                pid: *pid,
                pid_start: *pid_start,
            }),
            _ => None,
        };
        self.transition_connection(&request, next, marker, None)?;
        Ok(true)
    }

    async fn flush_connection_cancel(
        &self,
        request: &PendingConnectionRequest,
    ) -> Result<(), crate::AgentError> {
        let broker = self
            .connection_broker
            .as_ref()
            .ok_or_else(|| ConnectionRequestError::Unavailable("no connection broker".into()))?;
        let scope = request
            .scope
            .as_ref()
            .ok_or(ConnectionRequestError::Stale)?;
        let resolved = connection_io(broker.resolve(ResolveConnectionScope {
            scope_ref: scope.scope_ref.clone(),
            request_id: request.id.clone(),
            invocation_id: request.invocation.clone(),
            route_id: broker.route_id().into(),
        }))
        .await?;
        if resolved.scope_ref != scope.scope_ref
            || resolved.request_id != request.id
            || resolved.invocation_id != request.invocation
            || resolved.route_id != broker.route_id()
            || resolved.offer_id != request.requirement.offer_id
        {
            return Err(ConnectionRequestError::Uncertain.into());
        }
        if matches!(
            resolved.state,
            ConnectionScopeState::Pending | ConnectionScopeState::Ready
        ) {
            let cancelled = connection_io(broker.cancel(ClaimConnectionScope {
                scope_ref: scope.scope_ref.clone(),
                request_id: request.id.clone(),
                invocation_id: request.invocation.clone(),
                expected_broker_revision: resolved.broker_revision,
            }))
            .await?;
            if cancelled.scope_ref != scope.scope_ref
                || cancelled.request_id != request.id
                || cancelled.invocation_id != request.invocation
                || cancelled.route_id != broker.route_id()
                || cancelled.state != ConnectionScopeState::Cancelled
            {
                return Err(ConnectionRequestError::Uncertain.into());
            }
        }
        let mut acknowledged = request.clone();
        acknowledged.cancel_pending = false;
        acknowledged.local_revision = acknowledged
            .local_revision
            .checked_add(1)
            .ok_or(ConnectionRequestError::Stale)?;
        acknowledged.scope = None;
        acknowledged.resolution = None;
        self.store.compare_record(
            RECORD_KIND,
            &request.id,
            &serde_json::to_value(request)?,
            &serde_json::to_value(acknowledged)?,
        )?;
        Ok(())
    }

    pub fn recover_connection_requests(
        &self,
        session: Option<&str>,
    ) -> Result<usize, crate::AgentError> {
        let mut recovered = 0;
        for value in self.store.list_records(RECORD_KIND, session)? {
            let request: PendingConnectionRequest = serde_json::from_value(value)?;
            if matches!(&request.phase, ConnectionRequestPhase::Closed { reason } if reason == "cancelled")
            {
                recovered += usize::from(self.clear_dead_cancelled_owner(&request)?);
                continue;
            }
            if self.retire_connection_orphan(&request)? {
                continue;
            }
            if let ConnectionRequestPhase::Started {
                run_id,
                pid,
                pid_start,
            } = &request.phase
            {
                if !crate::process::writer_alive(*pid, *pid_start) {
                    let mut next = request.clone();
                    next.phase = ConnectionRequestPhase::Interrupted {
                        run_id: run_id.clone(),
                    };
                    self.transition_connection(&request, next, None, None)?;
                    recovered += 1;
                }
            }
        }
        Ok(recovered)
    }

    fn clear_dead_cancelled_owner(
        &self,
        request: &PendingConnectionRequest,
    ) -> Result<bool, crate::AgentError> {
        let admission = self.chat_admission.chat(&request.session);
        let _guard = admission.lock().unwrap();
        let Some(header) = self.store.session_version(&request.session)? else {
            return Ok(false);
        };
        let record: crate::session::SessionRecord = serde_json::from_str(&header)?;
        if record.id.as_str() != request.session
            || record.project != request.project
            || record.turn_count != request.turn + 1
            || request.work != format!("session:{}/{}", request.session, request.turn)
        {
            return Ok(false);
        }
        let Some(current) = self
            .store
            .list_session_turns(&request.session, Some(1))?
            .into_iter()
            .next()
        else {
            return Ok(false);
        };
        let mut turn: crate::session::Turn = serde_json::from_value(current.clone())?;
        let Some(TurnConnection::Started {
            request_id,
            pid,
            pid_start,
            ..
        }) = &turn.connection
        else {
            return Ok(false);
        };
        if request_id != &request.id
            || turn.index != request.turn
            || turn.pid.is_some()
            || turn.pid_start.is_some()
            || !turn.cancelled
            || crate::process::writer_alive(*pid, *pid_start)
        {
            return Ok(false);
        }
        turn.connection = None;
        let request_value = serde_json::to_value(request)?;
        self.store.transition_chat(ark_store::ChatTransition {
            session: &request.session,
            expected_header: &header,
            header: &serde_json::from_str::<serde_json::Value>(&header)?,
            index: request.turn,
            expected_turn: Some(&current),
            turn: &serde_json::to_value(turn)?,
            records: &[ark_store::RecordChange {
                kind: RECORD_KIND,
                id: &request.id,
                scope: &request.session,
                expected: Some(&request_value),
                value: &request_value,
            }],
            invocation: None,
            other_turn: None,
        })?;
        Ok(true)
    }
}
