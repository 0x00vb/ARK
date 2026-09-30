//! OpenAI-compatible chat completions (also serves any OpenAI-compatible endpoint via
//! `OPENAI_BASE_URL`). API-key auth.

use crate::stream::{for_each_line, sse_data, OpenAiFold};
use crate::{
    mangle_tool_name, unmangle_tool_name, Completion, Message, Provider, ProviderError, Result,
    Role, TextSink, ToolCall, ToolDef,
};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct OpenAi {
    base: String,
    model: String,
    key: String,
    client: reqwest::Client,
}

impl OpenAi {
    pub fn from_env(model: impl Into<String>) -> Result<Self> {
        let key = std::env::var("OPENAI_API_KEY")
            .map_err(|_| ProviderError::MissingCredential("OPENAI_API_KEY".into()))?;
        Ok(OpenAi {
            base: std::env::var("OPENAI_BASE_URL")
                .unwrap_or_else(|_| "https://api.openai.com/v1".into()),
            model: model.into(),
            key,
            client: reqwest::Client::new(),
        })
    }

    /// Construct explicitly against any OpenAI-compatible endpoint.
    pub fn new(base: impl Into<String>, model: impl Into<String>, key: impl Into<String>) -> Self {
        OpenAi {
            base: base.into(),
            model: model.into(),
            key: key.into(),
            client: reqwest::Client::new(),
        }
    }
}

fn role_str(r: Role) -> &'static str {
    match r {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

fn to_wire_message(m: &Message) -> Value {
    match m.role {
        Role::Tool => json!({
            "role": "tool",
            "tool_call_id": m.tool_call_id.clone().unwrap_or_default(),
            "content": m.content,
        }),
        Role::Assistant if !m.tool_calls.is_empty() => json!({
            "role": "assistant",
            "content": Value::Null,
            "tool_calls": m.tool_calls.iter().map(|c| json!({
                "id": c.id,
                "type": "function",
                "function": { "name": mangle_tool_name(&c.name), "arguments": c.arguments.to_string() }
            })).collect::<Vec<_>>(),
        }),
        _ => json!({ "role": role_str(m.role), "content": m.content }),
    }
}

#[async_trait]
impl Provider for OpenAi {
    fn describe(&self) -> String {
        format!("openai {}", self.model)
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
                provider: "openai".into(),
                detail: v["error"]["message"]
                    .as_str()
                    .unwrap_or("request failed")
                    .to_string(),
            });
        }

        let msg = &v["choices"][0]["message"];
        let content = msg
            .get("content")
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .to_string();
        let mut tool_calls = Vec::new();
        if let Some(calls) = msg.get("tool_calls").and_then(|c| c.as_array()) {
            for c in calls {
                let f = &c["function"];
                let name = f["name"].as_str().unwrap_or_default();
                let arguments = f
                    .get("arguments")
                    .and_then(|a| a.as_str())
                    .and_then(|s| serde_json::from_str(s).ok())
                    .unwrap_or(json!({}));
                tool_calls.push(ToolCall {
                    id: c["id"].as_str().unwrap_or("call_0").to_string(),
                    name: unmangle_tool_name(name),
                    arguments,
                });
            }
        }
        let usage = crate::stream::openai_usage(&v["usage"]).unwrap_or_default();
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
                provider: "openai".into(),
                detail: v["error"]["message"]
                    .as_str()
                    .unwrap_or("request failed")
                    .to_string(),
            });
        }
        let mut fold = OpenAiFold::default();
        for_each_line(resp, |line| match sse_data(line) {
            Some(data) => fold.feed(&serde_json::from_str::<Value>(data)?, on_text),
            None => Ok(()),
        })
        .await?;
        Ok(fold.finish())
    }
}

impl OpenAi {
    fn body(&self, system: &str, messages: &[Message], tools: &[ToolDef], stream: bool) -> Value {
        let mut wire: Vec<Value> = vec![json!({ "role": "system", "content": system })];
        wire.extend(messages.iter().map(to_wire_message));
        let tool_defs: Vec<Value> = tools
            .iter()
            .map(|t| json!({
                "type": "function",
                "function": { "name": mangle_tool_name(&t.name), "description": t.description, "parameters": t.parameters }
            }))
            .collect();
        let mut body = json!({ "model": self.model, "messages": wire, "tools": tool_defs, "temperature": 0.1 });
        if stream {
            body["stream"] = json!(true);
            body["stream_options"] = json!({ "include_usage": true });
        }
        body
    }

    async fn post(&self, body: &Value) -> Result<reqwest::Response> {
        Ok(self
            .client
            .post(format!("{}/chat/completions", self.base))
            .bearer_auth(&self.key)
            .json(body)
            .send()
            .await?)
    }
}
