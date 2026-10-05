//! # ark-model
//!
//! Bring-your-own-model. A single narrow interface — chat completion with tool-calling —
//! that the runtime talks to, so a local Ollama model and a hosted API are
//! interchangeable. Providers are kept generic: the runtime never knows which one
//! answered.
//!
//! Auth: local models (no auth), API keys, and subscriptions through the vendor's official
//! CLI ([`cli`]: Claude Code and Codex sign their own requests). ARK never reads a CLI's
//! credentials or scrapes a web session. [`catalog`] lists what is usable on this machine.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

mod anthropic;
pub mod capacity;
mod catalog;
pub mod cli;
mod codex_app;
pub mod keys;
mod live;
pub mod mock;
mod ollama;
mod openai;
pub mod resilience;
pub mod session;
mod stream;
pub mod usage;

pub use anthropic::Anthropic;
pub use capacity::{Capacity, CapacitySnapshot, Limited, Limiter};
pub use catalog::{catalog, Backend, BackendKind, PROVIDERS};
pub use cli::{Cli, CliModel, CliStatus};
pub use mock::MockProvider;
pub use ollama::Ollama;
pub use openai::OpenAi;
pub use resilience::{HttpConfig, RetryPolicy};
pub use session::{
    ManagedSession, SessionBackend, SessionEvent, SessionEventKind, SessionPolicy, SessionSnapshot,
    SessionState, SubscriptionRuntime,
};
pub use usage::{subscription_usage, SubscriptionUsage, UsageWindow};

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    /// The connection failed, timed out or broke.
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    /// The provider refused the request for a reason a retry does not change.
    #[error("{provider} error: {detail}")]
    Api { provider: String, detail: String },
    /// A non-success HTTP status.
    #[error("{provider} returned {status}: {detail}")]
    Status {
        provider: String,
        status: u16,
        detail: String,
        /// The server's `retry-after`, when it sent one.
        retry_after: Option<std::time::Duration>,
    },
    /// The provider reported a temporary failure inside a successful response (an
    /// `overloaded_error` event mid-stream).
    #[error("{provider} is overloaded: {detail}")]
    Overloaded { provider: String, detail: String },
    /// The reply could not be used as sent: a tool call whose arguments are not valid JSON
    /// (usually cut off). Running the tool with guessed arguments would be wrong.
    #[error("{provider} sent a malformed reply: {detail}")]
    Malformed { provider: String, detail: String },
    #[error("missing credential: {0}")]
    MissingCredential(String),
    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),
}

impl ProviderError {
    /// Whether the same request may succeed if sent again: network failures and timeouts,
    /// rate limits and server errors (408, 425, 429, 5xx), overload, and a malformed reply (a
    /// new generation may be well formed). A bad request, missing credentials or a refusal
    /// are not.
    pub fn is_transient(&self) -> bool {
        match self {
            ProviderError::Http(e) => {
                e.is_timeout() || e.is_connect() || e.is_request() || e.is_body()
            }
            ProviderError::Status { status, .. } => {
                matches!(status, 408 | 425 | 429) || (500..=599).contains(status)
            }
            ProviderError::Overloaded { .. } | ProviderError::Malformed { .. } => true,
            ProviderError::Api { .. }
            | ProviderError::MissingCredential(_)
            | ProviderError::Serde(_) => false,
        }
    }

    /// How long the server asked to wait, if it did.
    pub fn retry_after(&self) -> Option<std::time::Duration> {
        match self {
            ProviderError::Status { retry_after, .. } => *retry_after,
            _ => None,
        }
    }
}

pub type Result<T> = std::result::Result<T, ProviderError>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// A request from the model to run one tool.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// One message in the conversation. Carries tool calls (assistant) or a tool result
/// (tool), so the transcript round-trips across providers.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    pub tool_call_id: Option<String>,
    pub tool_name: Option<String>,
}

impl Message {
    pub fn user(content: impl Into<String>) -> Self {
        Self::text(Role::User, content)
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self::text(Role::Assistant, content)
    }
    fn text(role: Role, content: impl Into<String>) -> Self {
        Message {
            role,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_name: None,
        }
    }
    /// An assistant turn that requested tool calls.
    pub fn assistant_tool_calls(calls: Vec<ToolCall>) -> Self {
        Message {
            role: Role::Assistant,
            content: String::new(),
            tool_calls: calls,
            tool_call_id: None,
            tool_name: None,
        }
    }
    /// A tool result to feed back to the model.
    pub fn tool_result(call: &ToolCall, content: impl Into<String>) -> Self {
        Message {
            role: Role::Tool,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: Some(call.id.clone()),
            tool_name: Some(call.name.clone()),
        }
    }
}

/// A tool the model may call. `parameters` is a JSON Schema object.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// Token usage for one completion. The runtime treats tokens as a managed budget, so
/// every call reports what it cost. Providers fill this from the API's own accounting when
/// available; otherwise the runtime falls back to [`estimate_tokens`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// The whole prompt, cached part included.
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    /// True when the numbers came from the provider, false when estimated.
    pub measured: bool,
    /// The part of `prompt_tokens` the provider served from its prompt cache (0 when it
    /// reports nothing).
    #[serde(default)]
    pub cached_tokens: u32,
    /// The part of `prompt_tokens` written to the prompt cache on this call. Anthropic bills
    /// it above the normal input price; providers that cache for free report 0.
    #[serde(default)]
    pub cache_write_tokens: u32,
}

impl Usage {
    pub fn total(&self) -> u32 {
        self.prompt_tokens + self.completion_tokens
    }
    pub fn measured(prompt: u32, completion: u32) -> Self {
        Usage {
            prompt_tokens: prompt,
            completion_tokens: completion,
            measured: true,
            cached_tokens: 0,
            cache_write_tokens: 0,
        }
    }
    pub fn with_cached(mut self, cached: u32) -> Self {
        self.cached_tokens = cached;
        self
    }
    pub fn with_cache_write(mut self, written: u32) -> Self {
        self.cache_write_tokens = written;
        self
    }
    /// Prompt tokens neither read from nor written to a cache.
    pub fn uncached(&self) -> u32 {
        self.prompt_tokens
            .saturating_sub(self.cached_tokens)
            .saturating_sub(self.cache_write_tokens)
    }
}

/// A rough token estimate for text, used when a provider does not report usage.
/// ~4 characters per token is the usual English approximation; good enough for budgeting,
/// never used for billing.
pub fn estimate_tokens(text: &str) -> u32 {
    ((text.chars().count() as f32) / 4.0).ceil() as u32
}

/// The model's reply: free text and/or tool calls, plus what it cost.
#[derive(Clone, Debug, Default)]
pub struct Completion {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    pub usage: Usage,
}

/// One web search result from a provider's own search ([`Provider::web_search`]).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WebHit {
    pub title: String,
    pub url: String,
    /// A sentence about the page. Empty when the search gave none.
    #[serde(default)]
    pub snippet: String,
}

/// What a provider's search is asked for, and the shape its answer must have. Shared by the
/// providers that search through a model call (the CLIs).
pub(crate) fn search_prompt(query: &str, limit: usize) -> String {
    format!(
        "Search the web for: {query}\n\nReturn up to {limit} results from the search, each with \
         its title, url and one sentence from or about the page. Do not answer the question \
         yourself; only report what the search found."
    )
}

pub(crate) const SEARCH_SCHEMA: &str = r#"{"type":"object","additionalProperties":false,"properties":{"results":{"type":"array","items":{"type":"object","additionalProperties":false,"properties":{"title":{"type":"string"},"url":{"type":"string"},"snippet":{"type":"string"}},"required":["title","url","snippet"]}}},"required":["results"]}"#;

/// The hits of a reply shaped by [`SEARCH_SCHEMA`], at most `limit`, web links only.
pub(crate) fn hits_from(reply: &serde_json::Value, limit: usize) -> Vec<WebHit> {
    reply["results"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| serde_json::from_value::<WebHit>(r.clone()).ok())
        .filter(|h| h.url.starts_with("http://") || h.url.starts_with("https://"))
        .take(limit)
        .collect()
}

/// Receives incremental answer text while a streaming completion arrives. It is called from
/// inside the provider's future, so it must be cheap and must not block.
pub type TextSink<'a> = &'a (dyn Fn(&str) + Send + Sync);

/// A model backend. One required method; everything else is translation inside the impl.
#[async_trait]
pub trait Provider: Send + Sync {
    async fn complete(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> Result<Completion>;
    /// Like [`complete`](Provider::complete), but delivers answer text to `on_text` as it is
    /// generated. The returned [`Completion`] is the same as a non-streaming call would give.
    /// The default emits the whole text once, so every provider is usable by a streaming
    /// front-end; providers with a native streaming API override it.
    async fn complete_streaming(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDef],
        on_text: TextSink<'_>,
    ) -> Result<Completion> {
        let c = self.complete(system, messages, tools).await?;
        if !c.content.is_empty() {
            on_text(&c.content);
        }
        Ok(c)
    }
    /// Search the web with the vendor's own search. `None`: this provider has none. The
    /// default has none; wrappers forward to what they wrap.
    async fn web_search(&self, _query: &str, _limit: usize) -> Option<Result<Vec<WebHit>>> {
        None
    }
    /// Human description, e.g. "ollama qwen3:4b-instruct".
    fn describe(&self) -> String;
    /// The model identifier this provider targets (e.g. "qwen3:4b-instruct"). Used by
    /// metrics and the router; defaults to [`describe`](Provider::describe).
    fn model_id(&self) -> String {
        self.describe()
    }
}

/// Build a provider from the environment:
/// - `ARK_PROVIDER` = ollama (default) | openai | anthropic | claude | codex
/// - `ARK_MODEL` overrides the model
/// - API keys from `OPENAI_API_KEY` / `ANTHROPIC_API_KEY`, else a saved key ([`keys`])
pub fn from_env() -> Result<Box<dyn Provider>> {
    let which = std::env::var("ARK_PROVIDER").unwrap_or_else(|_| "ollama".into());
    let model = std::env::var("ARK_MODEL").ok();
    build_box(&which, model)
}

fn build_box(which: &str, model: Option<String>) -> Result<Box<dyn Provider>> {
    match which {
        "ollama" => Ok(Box::new(Ollama::new(
            model.unwrap_or_else(|| "qwen3:4b-instruct".into()),
        ))),
        "openai" => Ok(Box::new(OpenAi::from_env(
            model.unwrap_or_else(|| "gpt-4o-mini".into()),
        )?)),
        "anthropic" => Ok(Box::new(Anthropic::from_env(
            model.unwrap_or_else(|| "claude-sonnet-5-5".into()),
        )?)),
        "claude" => Ok(Box::new(CliModel::new(
            Cli::Claude,
            model.unwrap_or_default(),
        ))),
        "codex" => Ok(Box::new(CliModel::new(
            Cli::Codex,
            model.unwrap_or_default(),
        ))),
        "mock" => {
            let path = std::env::var("ARK_MOCK_SCRIPT")
                .map_err(|_| ProviderError::MissingCredential("ARK_MOCK_SCRIPT".into()))?;
            Ok(Box::new(MockProvider::from_script_file(
                std::path::Path::new(&path),
            )?))
        }
        other => Err(ProviderError::Api {
            provider: other.into(),
            detail: "unknown provider (use ollama|openai|anthropic|claude|codex|mock)".into(),
        }),
    }
}

/// Build a shared provider by name — the [`std::sync::Arc`] form the model router uses so
/// several roles can share one backend.
pub fn build(which: &str, model: Option<String>) -> Result<std::sync::Arc<dyn Provider>> {
    Ok(std::sync::Arc::from(build_box(which, model)?))
}

/// The environment default provider as a shared [`std::sync::Arc`].
pub fn from_env_arc() -> Result<std::sync::Arc<dyn Provider>> {
    let which = std::env::var("ARK_PROVIDER").unwrap_or_else(|_| "ollama".into());
    build(&which, std::env::var("ARK_MODEL").ok())
}

/// Sanitize an action name (`fs.read`) into a tool name providers accept (`fs__read`);
/// `unmangle` reverses it. Some tool-calling APIs forbid `.` in tool names.
pub fn mangle_tool_name(action: &str) -> String {
    action.replace('.', "__")
}
pub fn unmangle_tool_name(tool: &str) -> String {
    tool.replace("__", ".")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_name_mangling_roundtrips() {
        assert_eq!(mangle_tool_name("fs.read"), "fs__read");
        assert_eq!(unmangle_tool_name("fs__read"), "fs.read");
        assert_eq!(unmangle_tool_name(&mangle_tool_name("web.get")), "web.get");
    }

    #[test]
    fn tool_result_message_links_call() {
        let call = ToolCall {
            id: "call_1".into(),
            name: "fs.read".into(),
            arguments: serde_json::json!({}),
        };
        let m = Message::tool_result(&call, "{\"lines\":3}");
        assert!(matches!(m.role, Role::Tool));
        assert_eq!(m.tool_call_id.as_deref(), Some("call_1"));
        assert_eq!(m.tool_name.as_deref(), Some("fs.read"));
    }

    #[test]
    fn token_estimate_is_chars_over_four() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("abcde"), 2);
        assert_eq!(Usage::measured(10, 3).total(), 13);
    }

    #[tokio::test]
    async fn mock_provider_records_and_replays() {
        use mock::MockProvider;
        let p = MockProvider::new(
            "test-model",
            vec![Completion {
                content: "hi".into(),
                tool_calls: vec![],
                usage: Usage::default(),
            }],
        );
        let out = p.complete("sys", &[Message::user("q")], &[]).await.unwrap();
        assert_eq!(out.content, "hi");
        assert!(
            out.usage.measured,
            "mock fills usage when script leaves it default"
        );
        assert_eq!(p.call_count(), 1);
        assert_eq!(p.recorded()[0].system, "sys");
        assert_eq!(p.model_id(), "test-model");
    }

    #[test]
    fn unknown_provider_errors() {
        std::env::set_var("ARK_PROVIDER", "does-not-exist");
        assert!(from_env().is_err());
        std::env::remove_var("ARK_PROVIDER");
    }
}
