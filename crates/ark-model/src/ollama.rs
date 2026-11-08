//! Local models via Ollama's `/api/chat` (no auth). The default provider: private,
//! offline, and the one the agent loop is tested against.

use crate::resilience::{status_error, HttpConfig};
use crate::stream::{for_each_line, parse_args, OllamaFold};
use crate::{
    mangle_tool_name, Completion, Message, Provider, Result, Role, TextSink, ToolCall, ToolDef,
    Usage,
};
use async_trait::async_trait;
use serde_json::{json, Value};

/// Output cap when none is configured. Ollama has none by default, and with context shifting
/// a degenerate reply can run for as long as the server is up.
const DEFAULT_NUM_PREDICT: u32 = 4096;

pub struct Ollama {
    host: String,
    model: String,
    client: reqwest::Client,
    http: HttpConfig,
}

impl Ollama {
    pub fn new(model: impl Into<String>) -> Self {
        let http = HttpConfig::default();
        Ollama {
            host: std::env::var("OLLAMA_HOST").unwrap_or_else(|_| "http://localhost:11434".into()),
            model: model.into(),
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

    /// Talk to another Ollama server.
    pub fn with_host(mut self, host: impl Into<String>) -> Self {
        self.host = host.into();
        self
    }

    async fn post(&self, body: &Value, stream: bool) -> Result<reqwest::Response> {
        let mut req = self
            .client
            .post(format!("{}/api/chat", self.host))
            .json(body);
        if !stream {
            req = req.timeout(self.http.request_timeout);
        }
        Ok(req.send().await?)
    }
}

fn error_detail(v: &Value) -> Option<String> {
    v.get("error").and_then(|e| e.as_str()).map(str::to_string)
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
    if !m.pictures.is_empty() {
        obj["images"] = Value::Array(m.pictures.iter().map(|p| json!(p.base64())).collect());
    }
    if let Some(name) = &m.tool_name {
        // Ollama matches tool results to calls by name.
        obj["tool_name"] = json!(mangle_tool_name(name));
    }
    obj
}

#[async_trait]
impl Provider for Ollama {
    fn host_controls_tools(&self) -> bool {
        true
    }
    fn describe(&self) -> String {
        format!("ollama {}", self.model)
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
            return Err(status_error("ollama", &self.model, resp, error_detail).await);
        }
        let v: Value = resp.json().await?;
        let msg = &v["message"];
        let content = msg
            .get("content")
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .to_string();
        let mut tool_calls = Vec::new();
        if let Some(calls) = msg.get("tool_calls").and_then(|c| c.as_array()) {
            for (i, c) in calls.iter().enumerate() {
                let f = &c["function"];
                let name = f.get("name").and_then(|n| n.as_str()).unwrap_or_default();
                // Ollama returns arguments as an object; occasionally as a JSON string.
                let arguments = match f.get("arguments") {
                    Some(Value::String(s)) => parse_args("ollama", name, s)?,
                    Some(other) => other.clone(),
                    None => json!({}),
                };
                tool_calls.push(ToolCall {
                    id: format!("call_{i}"),
                    name: crate::unmangle_tool_name(name),
                    arguments,
                });
            }
        }
        let usage = match (
            v.get("prompt_eval_count").and_then(|c| c.as_u64()),
            v.get("eval_count").and_then(|c| c.as_u64()),
        ) {
            (Some(p), Some(c)) => Usage::measured(p as u32, c as u32),
            _ => Usage::default(),
        };
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
            return Err(status_error("ollama", &self.model, resp, error_detail).await);
        }
        let mut fold = OllamaFold::default();
        for_each_line(resp, |line| {
            if line.trim().is_empty() {
                return Ok(());
            }
            fold.feed(&serde_json::from_str::<Value>(line)?, on_text)
        })
        .await?;
        Ok(fold.finish())
    }
}

impl Ollama {
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
        json!({
            "model": self.model,
            "messages": wire,
            "tools": tool_defs,
            "stream": stream,
            "options": {
                "temperature": 0.1,
                "num_predict": self.http.max_output_tokens.unwrap_or(DEFAULT_NUM_PREDICT),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Picture, ToolCall};

    #[test]
    fn a_picture_goes_in_the_messages_images() {
        let call = ToolCall {
            id: "t1".into(),
            name: "app.see".into(),
            arguments: json!({}),
        };
        let pic = Picture {
            media_type: "image/png".into(),
            bytes: std::sync::Arc::new(b"png".to_vec()),
        };
        let w = to_wire_message(&Message::tool_result(&call, "a window").with_pictures(vec![pic]));
        assert_eq!(w["images"], json!(["cG5n"]));
        assert!(to_wire_message(&Message::user("x")).get("images").is_none());
    }
}
