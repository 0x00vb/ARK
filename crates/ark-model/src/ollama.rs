//! Local models via Ollama's `/api/chat` (no auth). The default provider: private,
//! offline, and the one the agent loop is tested against.

use crate::{mangle_tool_name, Completion, Message, Provider, ProviderError, Result, Role, ToolCall, ToolDef, Usage};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct Ollama {
    host: String,
    model: String,
    client: reqwest::Client,
}

impl Ollama {
    pub fn new(model: impl Into<String>) -> Self {
        Ollama {
            host: std::env::var("OLLAMA_HOST").unwrap_or_else(|_| "http://localhost:11434".into()),
            model: model.into(),
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
    let mut obj = json!({ "role": role_str(m.role), "content": m.content });
    if !m.tool_calls.is_empty() {
        obj["tool_calls"] = Value::Array(
            m.tool_calls
                .iter()
                .map(|c| json!({ "function": { "name": mangle_tool_name(&c.name), "arguments": c.arguments } }))
                .collect(),
        );
    }
    if let Some(name) = &m.tool_name {
        // Ollama matches tool results to calls by name.
        obj["tool_name"] = json!(mangle_tool_name(name));
    }
    obj
}

#[async_trait]
impl Provider for Ollama {
    fn describe(&self) -> String {
        format!("ollama {}", self.model)
    }
    fn model_id(&self) -> String {
        self.model.clone()
    }

    async fn complete(&self, system: &str, messages: &[Message], tools: &[ToolDef]) -> Result<Completion> {
        let mut wire: Vec<Value> = vec![json!({ "role": "system", "content": system })];
        wire.extend(messages.iter().map(to_wire_message));

        let tool_defs: Vec<Value> = tools
            .iter()
            .map(|t| json!({
                "type": "function",
                "function": { "name": mangle_tool_name(&t.name), "description": t.description, "parameters": t.parameters }
            }))
            .collect();

        let body = json!({
            "model": self.model,
            "messages": wire,
            "tools": tool_defs,
            "stream": false,
            "options": { "temperature": 0.1 }
        });

        let resp = self
            .client
            .post(format!("{}/api/chat", self.host))
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        let v: Value = resp.json().await?;
        if !status.is_success() {
            return Err(ProviderError::Api {
                provider: "ollama".into(),
                detail: v.get("error").and_then(|e| e.as_str()).unwrap_or("request failed").to_string(),
            });
        }

        let msg = &v["message"];
        let content = msg.get("content").and_then(|c| c.as_str()).unwrap_or("").to_string();
        let mut tool_calls = Vec::new();
        if let Some(calls) = msg.get("tool_calls").and_then(|c| c.as_array()) {
            for (i, c) in calls.iter().enumerate() {
                let f = &c["function"];
                let name = f.get("name").and_then(|n| n.as_str()).unwrap_or_default();
                // Ollama returns arguments as an object; occasionally as a JSON string.
                let arguments = match f.get("arguments") {
                    Some(Value::String(s)) => serde_json::from_str(s).unwrap_or(json!({})),
                    Some(other) => other.clone(),
                    None => json!({}),
                };
                tool_calls.push(ToolCall { id: format!("call_{i}"), name: crate::unmangle_tool_name(name), arguments });
            }
        }
        let usage = match (v.get("prompt_eval_count").and_then(|c| c.as_u64()), v.get("eval_count").and_then(|c| c.as_u64())) {
            (Some(p), Some(c)) => Usage::measured(p as u32, c as u32),
            _ => Usage::default(),
        };
        Ok(Completion { content, tool_calls, usage })
    }
}
