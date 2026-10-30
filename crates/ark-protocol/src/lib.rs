//! The additive wire contract for separately released ARK clients.
//!
//! This crate contains data only. It has no transport, runtime, UI or consumer dependency.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const CURRENT_MAJOR: u16 = 1;
pub const CURRENT_MINOR: u16 = 9;
pub const TERMINAL_WRITE_BYTES: u64 = 16 * 1024;
pub const TERMINAL_REPLAY_BYTES: u64 = 256 * 1024;

#[derive(Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativePushRegistrationRequest {
    #[schemars(length(min = 36, max = 36))]
    pub registration_id: String,
    #[schemars(length(min = 1, max = 200))]
    pub runtime_route_id: String,
    #[schemars(length(min = 20, max = 40))]
    pub expires_at: String,
    #[schemars(length(min = 43, max = 43))]
    pub send_capability: String,
}

impl std::fmt::Debug for NativePushRegistrationRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativePushRegistrationRequest")
            .field("send_capability", &"[redacted]")
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativePushRegistrationResponse {
    pub registration_id: String,
    pub runtime_route_id: String,
    pub expires_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativePushRemoveRequest {
    #[schemars(length(min = 36, max = 36))]
    pub registration_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativePushPresenceRequest {
    #[schemars(length(min = 36, max = 36))]
    pub registration_id: String,
    pub in_foreground: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct NativePushResolution {
    pub attention_id: String,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item_id: Option<String>,
}

macro_rules! wire {
    ($(#[$meta:meta])* $vis:vis struct $name:ident { $($(#[$field_meta:meta])* $field:ident: $ty:ty),* $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
        #[serde(rename_all = "camelCase")]
        $vis struct $name { $($(#[$field_meta])* pub $field: $ty),* }
    };
}

wire!(
    pub struct Bootstrap {
        protocols: Vec<ProtocolVersion>,
        runtime_version: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        update_url: Option<String>,
    }
);

impl Bootstrap {
    pub fn current(runtime_version: impl Into<String>) -> Self {
        Self {
            protocols: vec![ProtocolVersion {
                major: CURRENT_MAJOR,
                latest_minor: CURRENT_MINOR,
            }],
            runtime_version: runtime_version.into(),
            update_url: Some("https://aiark.dev/download".into()),
        }
    }
}

wire!(
    pub struct ProtocolVersion {
        major: u16,
        latest_minor: u16,
    }
);
wire!(
    pub struct PairRequest {
        code: String,
        device_name: String,
        installation_nonce: String,
    }
);
wire!(
    pub struct AccountPairRequest {
        #[schemars(length(min = 1, max = 200))]
        route_id: String,
        #[schemars(length(min = 1, max = 80))]
        device_name: String,
        #[schemars(length(min = 1, max = 200))]
        installation_id: String,
    }
);
wire!(
    pub struct PairResponse {
        device_id: String,
        credential: String,
        expires_at: String,
        mutation_epoch: String,
        next_mutation_counter: DecimalString,
    }
);
wire!(
    pub struct Hello {
        protocol_major: u16,
        protocol_minor: u16,
        minimum_client_minor: u16,
        capabilities: Vec<String>,
        limits: Limits,
        mutation_epoch: String,
        next_mutation_counter: DecimalString,
        #[serde(skip_serializing_if = "Option::is_none")]
        route_id: Option<String>,
    }
);
wire!(
    pub struct Limits {
        page_size: u64,
        event_bytes: u64,
        upload_part_bytes: u64,
        upload_bytes: u64,
        terminal_write_bytes: u64,
        terminal_replay_bytes: u64,
    }
);

#[derive(
    Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash, PartialOrd, Ord,
)]
#[serde(transparent)]
#[schemars(transparent)]
pub struct DecimalString(String);

impl DecimalString {
    pub fn new(value: impl Into<String>) -> Result<Self, &'static str> {
        let value = value.into();
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err("a decimal string contains only ASCII digits");
        }
        let normalized = value.trim_start_matches('0');
        Ok(Self(if normalized.is_empty() {
            "0".into()
        } else {
            normalized.into()
        }))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn as_u64(&self) -> Result<u64, std::num::ParseIntError> {
        self.0.parse()
    }
}

wire!(
    pub struct ErrorEnvelope {
        code: String,
        display: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        retry_after_seconds: Option<u64>,
    }
);
wire!(
    pub struct ProviderConnectionSummary {
        id: String,
        title: String,
        state: String,
        display: String,
    }
);
wire!(
    pub struct ProviderConnectionsSnapshot {
        route_id: String,
        connections: Vec<ProviderConnectionSummary>,
    }
);
wire!(
    pub struct ProviderAuthorizationBeginRequest {
        #[schemars(length(min = 1, max = 200))]
        route_id: String,
        #[schemars(length(min = 1, max = 128))]
        request_id: String,
    }
);
#[derive(Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProviderAuthorizationCodeRequest {
    #[schemars(length(min = 1, max = 4096))]
    pub code: String,
}

impl std::fmt::Debug for ProviderAuthorizationCodeRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderAuthorizationCodeRequest")
            .field("code", &"[redacted]")
            .finish()
    }
}

#[derive(Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProviderAuthorizationAttempt {
    pub id: String,
    pub connection_id: String,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authorization_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_code: Option<String>,
    pub expires_at: String,
    pub display: String,
}

impl std::fmt::Debug for ProviderAuthorizationAttempt {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderAuthorizationAttempt")
            .field("id", &self.id)
            .field("connection_id", &self.connection_id)
            .field("state", &self.state)
            .field(
                "authorization_url",
                &self.authorization_url.as_ref().map(|_| "[redacted]"),
            )
            .field("user_code", &self.user_code.as_ref().map(|_| "[redacted]"))
            .field("expires_at", &self.expires_at)
            .field("display", &self.display)
            .finish()
    }
}
wire!(
    pub struct ModelEffort {
        id: String,
        title: String,
    }
);
wire!(
    pub struct ModelSelection {
        spec: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        effort_id: Option<String>,
    }
);
wire!(
    pub struct ModelSummary {
        spec: String,
        title: String,
        efforts: Vec<ModelEffort>,
        #[serde(skip_serializing_if = "Option::is_none")]
        default_effort_id: Option<String>,
    }
);
wire!(
    pub struct ModelProvider {
        id: String,
        title: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        connection_id: Option<String>,
        models: Vec<ModelSummary>,
    }
);
wire!(
    pub struct ModelDefault {
        #[serde(skip_serializing_if = "Option::is_none")]
        selection: Option<ModelSelection>,
        display: String,
    }
);
wire!(
    pub struct ModelsSnapshot {
        route_id: String,
        default_model: ModelDefault,
        providers: Vec<ModelProvider>,
    }
);
wire!(
    pub struct ConversationModel {
        #[serde(skip_serializing_if = "Option::is_none")]
        selection: Option<ModelSelection>,
        display: String,
        can_select: bool,
    }
);
wire!(
    pub struct ChatModelInput {
        model: ModelSelection,
    }
);
wire!(
    pub struct ApprovalChoice {
        id: String,
        label: String,
        system_image: Option<String>,
        destructive: bool,
    }
);
wire!(
    pub struct AttentionItem {
        id: String,
        kind: String,
        title: String,
        detail: String,
        teammate: String,
        project: String,
        computer: String,
        choices: Vec<ApprovalChoice>,
    }
);
wire!(
    pub struct WorkingItem {
        id: String,
        title: String,
        step: String,
        teammate: String,
        computer: String,
        elapsed_seconds: u64,
        phase: String,
        chat_id: String,
    }
);
wire!(
    pub struct ResultSummary {
        id: String,
        title: String,
        project: String,
        created_at: String,
        kind: String,
        byte_count: Option<u64>,
        preview_text: Option<String>,
        download_path: Option<String>,
    }
);
wire!(
    pub struct ConversationAgent {
        name: String,
        role: String,
    }
);
wire!(
    pub struct ChatSummary {
        #[serde(skip_serializing_if = "Option::is_none")]
        agent: Option<ConversationAgent>,
        #[serde(skip_serializing_if = "Option::is_none")]
        participants: Option<Vec<String>>,
        id: String,
        title: String,
        project: String,
        computer: String,
        updated_at: String,
        preview: String,
        unread: bool,
    }
);
wire!(
    pub struct ChatMessage {
        id: String,
        role: String,
        author: String,
        body: String,
        created_at: String,
        delivery: Option<String>,
    }
);
wire!(
    pub struct ConversationSnapshot {
        chat: ChatSummary,
        messages: Vec<ChatMessage>,
        approvals: Vec<AttentionItem>,
        results: Vec<ResultSummary>,
        active_turn_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        model: Option<ConversationModel>,
    }
);
wire!(
    pub struct HomeSnapshot {
        sequence: DecimalString,
        attention: Vec<AttentionItem>,
        working: Vec<WorkingItem>,
        recent: Vec<ResultSummary>,
        most_recent_chat: Option<ChatSummary>,
    }
);
wire!(
    pub struct ChatsSnapshot {
        sequence: DecimalString,
        chats: Vec<ChatSummary>,
    }
);
wire!(
    pub struct NewChatRequest {
        project: String,
        request_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        model: Option<ModelSelection>,
    }
);
wire!(
    pub struct MutationRequest {
        epoch: String,
        counter: DecimalString,
        operation: String,
        target: String,
        input_digest: String,
        input: serde_json::Value,
    }
);
wire!(
    pub struct MutationReceipt {
        id: String,
        status: String,
        accepted_at: String,
        display: String,
    }
);

wire!(
    pub struct ProjectSummary {
        id: String,
        name: String,
        computer: String,
        has_folder: bool,
    }
);
wire!(
    pub struct ProjectsSnapshot {
        sequence: DecimalString,
        projects: Vec<ProjectSummary>,
    }
);
wire!(
    pub struct ComputerSummary {
        name: String,
        state: String,
        project_terminal: bool,
        unavailable_reason: Option<String>,
    }
);
wire!(
    pub struct ComputersSnapshot {
        here: String,
        computers: Vec<ComputerSummary>,
    }
);

wire!(
    pub struct TerminalOpenRequest {
        project_id: String,
        computer: Option<String>,
        #[schemars(range(min = 2, max = 1000))]
        cols: u16,
        #[schemars(range(min = 2, max = 1000))]
        rows: u16,
        copy_large_project: bool,
    }
);
wire!(
    pub struct TerminalOpenResponse {
        handle: String,
        generation: String,
        cwd: String,
        history: String,
        cursor: DecimalString,
        ended: bool,
        reattached: bool,
        project: ProjectSummary,
        computer: String,
    }
);
wire!(
    pub struct TerminalReadRequest {
        handle: String,
        generation: String,
        after: DecimalString,
    }
);
wire!(
    pub struct TerminalReadResponse {
        generation: String,
        data: String,
        base: DecimalString,
        cursor: DecimalString,
        gap: bool,
        ended: bool,
    }
);
wire!(
    pub struct TerminalWriteRequest {
        handle: String,
        generation: String,
        sequence: DecimalString,
        #[schemars(length(max = 16384))]
        data: String,
    }
);
wire!(
    pub struct TerminalResizeRequest {
        handle: String,
        generation: String,
        #[schemars(range(min = 2, max = 1000))]
        cols: u16,
        #[schemars(range(min = 2, max = 1000))]
        rows: u16,
    }
);
wire!(
    pub struct TerminalEndRequest {
        handle: String,
        generation: String,
    }
);
wire!(
    pub struct TerminalOperationResponse {
        accepted: bool,
    }
);
wire!(
    pub struct TerminalEvent {
        handle: String,
        generation: String,
        cursor: Option<DecimalString>,
        data: Option<String>,
        gap: bool,
        ended: bool,
    }
);
wire!(
    pub struct EventEnvelope {
        id: String,
        sequence: DecimalString,
        #[serde(rename = "type")]
        #[schemars(rename = "type")]
        event_type: String,
        affects: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        terminal: Option<TerminalEvent>,
    }
);

/// A schema root that names every type a separately released client may receive or send.
#[derive(JsonSchema)]
#[allow(dead_code)]
pub struct ProtocolSchema {
    native_push_registration_request: NativePushRegistrationRequest,
    native_push_registration_response: NativePushRegistrationResponse,
    native_push_remove_request: NativePushRemoveRequest,
    native_push_presence_request: NativePushPresenceRequest,
    native_push_resolution: NativePushResolution,
    bootstrap: Bootstrap,
    pair_request: PairRequest,
    account_pair_request: AccountPairRequest,
    pair_response: PairResponse,
    hello: Hello,
    error: ErrorEnvelope,
    provider_connections: ProviderConnectionsSnapshot,
    provider_authorization_begin: ProviderAuthorizationBeginRequest,
    provider_authorization_code: ProviderAuthorizationCodeRequest,
    provider_authorization_attempt: ProviderAuthorizationAttempt,
    models: ModelsSnapshot,
    model_provider: ModelProvider,
    model_summary: ModelSummary,
    model_effort: ModelEffort,
    model_selection: ModelSelection,
    model_default: ModelDefault,
    conversation_model: ConversationModel,
    chat_model_input: ChatModelInput,
    home: HomeSnapshot,
    chats: ChatsSnapshot,
    conversation: ConversationSnapshot,
    new_chat: NewChatRequest,
    mutation_request: MutationRequest,
    mutation_receipt: MutationReceipt,
    projects: ProjectsSnapshot,
    computers: ComputersSnapshot,
    terminal_open_request: TerminalOpenRequest,
    terminal_open_response: TerminalOpenResponse,
    terminal_read_request: TerminalReadRequest,
    terminal_read_response: TerminalReadResponse,
    terminal_write_request: TerminalWriteRequest,
    terminal_resize_request: TerminalResizeRequest,
    terminal_end_request: TerminalEndRequest,
    terminal_operation_response: TerminalOperationResponse,
    event: EventEnvelope,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn participant_identity_is_additive_and_preserves_unknown() {
        let legacy = serde_json::json!({"id":"chat", "title":"Research", "project":"P", "computer":"Mac", "updatedAt":"now", "preview":"", "unread":false});
        let unknown: ChatSummary = serde_json::from_value(legacy.clone()).unwrap();
        assert_eq!(unknown.participants, None);
        let mut group = legacy;
        group["participants"] = serde_json::json!(["Editor", "Writer"]);
        let known: ChatSummary = serde_json::from_value(group).unwrap();
        assert_eq!(
            known.participants,
            Some(vec!["Editor".into(), "Writer".into()])
        );
    }

    #[test]
    fn provider_authorization_debug_output_redacts_secrets() {
        let code = ProviderAuthorizationCodeRequest {
            code: "private-code".into(),
        };
        let attempt = ProviderAuthorizationAttempt {
            id: "attempt".into(),
            connection_id: "claude".into(),
            state: "waiting_for_code".into(),
            authorization_url: Some("https://claude.com/cai/oauth/authorize?state=private".into()),
            user_code: Some("PRIVATE-CODE".into()),
            expires_at: "2026-10-23T00:00:00Z".into(),
            display: "Continue".into(),
        };
        assert!(!format!("{code:?}").contains("private-code"));
        assert!(!format!("{attempt:?}").contains("state=private"));
        assert!(!format!("{attempt:?}").contains("PRIVATE-CODE"));
    }

    #[test]
    fn schema_includes_route_models_and_optional_conversation_selection() {
        let schema = serde_json::to_value(schemars::schema_for!(ProtocolSchema)).unwrap();
        assert!(schema["$defs"]["ModelsSnapshot"].is_object());
        assert!(schema["$defs"]["ModelSelection"].is_object());
        assert!(schema["$defs"]["ConversationModel"].is_object());
        assert!(
            schema["$defs"]["ProviderAuthorizationAttempt"]["properties"]["userCode"].is_object()
        );
        let required = schema["$defs"]["ConversationSnapshot"]["required"]
            .as_array()
            .unwrap();
        assert!(!required.iter().any(|field| field == "model"));
    }

    #[test]
    fn bootstrap_fixture_is_stable() {
        let actual = serde_json::to_value(Bootstrap::current("0.18.3")).unwrap();
        let expected: serde_json::Value =
            serde_json::from_str(include_str!("../fixtures/v1/bootstrap.json")).unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn terminal_fixtures_decode_and_keep_decimal_cursors() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../fixtures/v1/terminal.json")).unwrap();
        serde_json::from_value::<TerminalOpenRequest>(fixture["openRequest"].clone()).unwrap();
        serde_json::from_value::<TerminalOpenResponse>(fixture["openResponse"].clone()).unwrap();
        serde_json::from_value::<TerminalReadRequest>(fixture["readRequest"].clone()).unwrap();
        serde_json::from_value::<TerminalReadResponse>(fixture["readResponse"].clone()).unwrap();
        serde_json::from_value::<TerminalWriteRequest>(fixture["writeRequest"].clone()).unwrap();
        serde_json::from_value::<TerminalResizeRequest>(fixture["resizeRequest"].clone()).unwrap();
        serde_json::from_value::<TerminalEndRequest>(fixture["endRequest"].clone()).unwrap();
        serde_json::from_value::<EventEnvelope>(fixture["event"].clone()).unwrap();
        serde_json::from_value::<ErrorEnvelope>(fixture["error"].clone()).unwrap();
        assert_eq!(fixture["bounds"]["writeBytes"], TERMINAL_WRITE_BYTES);
        assert_eq!(fixture["bounds"]["replayBytes"], TERMINAL_REPLAY_BYTES);
    }

    #[test]
    fn counters_do_not_become_json_numbers() {
        let counter = DecimalString::new("18446744073709551615").unwrap();
        assert_eq!(
            serde_json::to_string(&counter).unwrap(),
            "\"18446744073709551615\""
        );
    }
}
