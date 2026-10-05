//! OpenAI-compatible chat completions (also serves any OpenAI-compatible endpoint via
//! `OPENAI_BASE_URL`). API-key auth.

use crate::resilience::{status_error, HttpConfig};
use crate::stream::{for_each_line, parse_args, sse_data, OpenAiFold};
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
    http: HttpConfig,
}

impl OpenAi {
    pub fn from_env(model: impl Into<String>) -> Result<Self> {
        let key = crate::keys::api_key("OPENAI_API_KEY")
            .ok_or_else(|| ProviderError::MissingCredential("OPENAI_API_KEY".into()))?;
        Ok(OpenAi::new(
            std::env::var("OPENAI_BASE_URL").unwrap_or_else(|_| "https://api.openai.com/v1".into()),
            model,
            key,
        ))
    }

    /// Construct explicitly against any OpenAI-compatible endpoint.
    pub fn new(base: impl Into<String>, model: impl Into<String>, key: impl Into<String>) -> Self {
        let http = HttpConfig::default();
        OpenAi {
            base: base.into(),
            model: model.into(),
            key: key.into(),
            client: http.client(),
            http,
        }
    }

    /// Use these timeouts and output cap.
    pub fn with_http(mut self, http: HttpConfig) -> Self {
        self.client = http.client();
        self.http = http;
        self
    }
}

fn error_detail(v: &Value) -> Option<String> {
    v["error"]["message"].as_str().map(str::to_string)
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
        let resp = self.post(&body, false).await?;
        if !resp.status().is_success() {
            return Err(status_error("openai", resp, error_detail).await);
        }
        let v: Value = resp.json().await?;
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
                let arguments = parse_args(
                    "openai",
                    name,
                    f.get("arguments").and_then(|a| a.as_str()).unwrap_or(""),
                )?;
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
        let resp = self.post(&body, true).await?;
        if !resp.status().is_success() {
            return Err(status_error("openai", resp, error_detail).await);
        }
        let mut fold = OpenAiFold::default();
        for_each_line(resp, |line| match sse_data(line) {
            Some(data) => fold.feed(&serde_json::from_str::<Value>(data)?, on_text),
            None => Ok(()),
        })
        .await?;
        fold.finish()
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
        if let Some(n) = self.http.max_output_tokens {
            body["max_tokens"] = json!(n);
        }
        if stream {
            body["stream"] = json!(true);
            body["stream_options"] = json!({ "include_usage": true });
        }
        body
    }

    async fn post(&self, body: &Value, stream: bool) -> Result<reqwest::Response> {
        let mut req = self
            .client
            .post(format!("{}/chat/completions", self.base))
            .bearer_auth(&self.key)
            .json(body);
        if !stream {
            req = req.timeout(self.http.request_timeout);
        }
        Ok(req.send().await?)
    }
}
