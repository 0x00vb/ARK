//! Anthropic Messages API. API-key auth. Content is block-structured: assistant tool
//! requests are `tool_use` blocks and results are `tool_result` blocks.

use crate::stream::{for_each_line, sse_data, AnthropicFold};
use crate::{
    mangle_tool_name, unmangle_tool_name, Completion, Message, Provider, ProviderError, Result,
    Role, TextSink, ToolCall, ToolDef,
};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct Anthropic {
    base: String,
    model: String,
    key: String,
    client: reqwest::Client,
}

impl Anthropic {
    pub fn from_env(model: impl Into<String>) -> Result<Self> {
        let key = std::env::var("ANTHROPIC_API_KEY")
            .map_err(|_| ProviderError::MissingCredential("ANTHROPIC_API_KEY".into()))?;
        Ok(Anthropic {
            base: std::env::var("ANTHROPIC_BASE_URL")
                .unwrap_or_else(|_| "https://api.anthropic.com/v1".into()),
            model: model.into(),
            key,
            client: reqwest::Client::new(),
        })
    }

    /// Construct explicitly (for consumers wiring their own credentials).
    pub fn new(model: impl Into<String>, key: impl Into<String>) -> Self {
        Anthropic {
            base: "https://api.anthropic.com/v1".into(),
            model: model.into(),
            key: key.into(),
            client: reqwest::Client::new(),
        }
    }
}

/// Anthropic has no `tool` role; results go in a `user` message as a `tool_result` block,
/// and tool requests are `tool_use` blocks in an assistant message. Translate our flat
/// messages accordingly.
fn to_wire_message(m: &Message) -> Value {
    match m.role {
        Role::Tool => json!({
            "role": "user",
            "content": [ { "type": "tool_result", "tool_use_id": m.tool_call_id.clone().unwrap_or_default(), "content": m.content } ]
        }),
        Role::Assistant if !m.tool_calls.is_empty() => {
            let mut blocks = Vec::new();
            if !m.content.is_empty() {
                blocks.push(json!({ "type": "text", "text": m.content }));
            }
            for c in &m.tool_calls {
                blocks.push(json!({ "type": "tool_use", "id": c.id, "name": mangle_tool_name(&c.name), "input": c.arguments }));
            }
            json!({ "role": "assistant", "content": blocks })
        }
        Role::Assistant => json!({ "role": "assistant", "content": m.content }),
        // System is passed as a top-level field, not a message; anything else is a user turn.
        _ => json!({ "role": "user", "content": m.content }),
    }
}

#[async_trait]
impl Provider for Anthropic {
    fn describe(&self) -> String {
        format!("anthropic {}", self.model)
    }
    fn model_id(&self) -> String {
        self.model.clone()
    }

    async fn complete(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> Result<Completion> {
        let body = self.body(system, messages, tools, false);
        let resp = self.post(&body).await?;
        let status = resp.status();
        let v: Value = resp.json().await?;
        if !status.is_success() {
            return Err(ProviderError::Api {
                provider: "anthropic".into(),
                detail: v["error"]["message"]
                    .as_str()
                    .unwrap_or("request failed")
                    .to_string(),
            });
        }

        let mut content = String::new();
        let mut tool_calls = Vec::new();
        if let Some(blocks) = v["content"].as_array() {
            for b in blocks {
                match b["type"].as_str() {
                    Some("text") => content.push_str(b["text"].as_str().unwrap_or_default()),
                    Some("tool_use") => tool_calls.push(ToolCall {
                        id: b["id"].as_str().unwrap_or("call_0").to_string(),
                        name: unmangle_tool_name(b["name"].as_str().unwrap_or_default()),
                        arguments: b.get("input").cloned().unwrap_or(json!({})),
                    }),
                    _ => {}
                }
            }
        }
        let usage = crate::stream::anthropic_usage(&v["usage"], None);
        Ok(Completion {
            content,
            tool_calls,
            usage,
        })
    }

    async fn complete_streaming(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDef],
        on_text: TextSink<'_>,
    ) -> Result<Completion> {
        let body = self.body(system, messages, tools, true);
        let resp = self.post(&body).await?;
        if !resp.status().is_success() {
            let v: Value = resp.json().await.unwrap_or_default();
            return Err(ProviderError::Api {
                provider: "anthropic".into(),
                detail: v["error"]["message"]
                    .as_str()
                    .unwrap_or("request failed")
                    .to_string(),
            });
        }
        let mut fold = AnthropicFold::default();
        for_each_line(resp, |line| match sse_data(line) {
            Some(data) => fold.feed(&serde_json::from_str::<Value>(data)?, on_text),
            None => Ok(()),
        })
        .await?;
        Ok(fold.finish())
    }
}

impl Anthropic {
    fn body(&self, system: &str, messages: &[Message], tools: &[ToolDef], stream: bool) -> Value {
        let wire: Vec<Value> = messages.iter().map(to_wire_message).collect();
        let tool_defs: Vec<Value> = tools
            .iter()
            .map(|t| json!({ "name": mangle_tool_name(&t.name), "description": t.description, "input_schema": t.parameters }))
            .collect();
        json!({
            "model": self.model,
            "max_tokens": 2048,
            "system": system,
            "messages": wire,
            "tools": tool_defs,
            "stream": stream,
        })
    }

    async fn post(&self, body: &Value) -> Result<reqwest::Response> {
        Ok(self
            .client
            .post(format!("{}/messages", self.base))
            .header("x-api-key", &self.key)
            .header("anthropic-version", "2023-06-01")
            .json(body)
            .send()
            .await?)
    }
}
