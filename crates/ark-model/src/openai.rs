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
    /// The provider name it reports (`openai`, or a named endpoint's).
    name: String,
    base: String,
    model: String,
    key: String,
    client: reqwest::Client,
    http: HttpConfig,
    /// The model reads images: OpenAI's own API, not a named endpoint unless told.
    pictures: bool,
    /// Input tokens a request may have, when known ([`Provider::context_window`]).
    window: Option<u32>,
}

impl OpenAi {
    pub fn from_env(model: impl Into<String>) -> Result<Self> {
        let model: String = model.into();
        let model_name = model.clone();
        let key = crate::keys::api_key("OPENAI_API_KEY")
            .ok_or_else(|| ProviderError::MissingCredential("OPENAI_API_KEY".into()))?;
        Ok(OpenAi::new(
            std::env::var("OPENAI_BASE_URL").unwrap_or_else(|_| "https://api.openai.com/v1".into()),
            model,
            key,
        )
        .reads_pictures(true)
        .with_context_window(openai_window(&model_name)))
    }

    /// Construct explicitly against any OpenAI-compatible endpoint.
    pub fn new(base: impl Into<String>, model: impl Into<String>, key: impl Into<String>) -> Self {
        let http = HttpConfig::default();
        OpenAi {
            name: "openai".into(),
            base: base.into(),
            model: model.into(),
            key: key.into(),
            client: http.client(),
            http,
            pictures: false,
            window: None,
        }
    }

    /// The input tokens a request to this model may have, when known.
    pub fn with_context_window(mut self, tokens: Option<u32>) -> Self {
        self.window = tokens;
        self
    }

    /// Whether the model reads images ([`Provider::reads_pictures`]).
    pub fn reads_pictures(mut self, yes: bool) -> Self {
        self.pictures = yes;
        self
    }

    /// Report itself as `name` (in `describe` and errors): a named endpoint's provider name.
    pub fn named(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
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

/// One message on the wire, and a tool result's pictures after it: a `tool` message takes only
/// text, so the pictures go in a `user` message that says where they come from.
fn to_wire(m: &Message) -> Vec<Value> {
    let mut out = vec![to_wire_message(m)];
    if !m.pictures.is_empty() {
        let mut parts = vec![json!({
            "type": "text",
            "text": format!("The picture from {}:", m.tool_name.as_deref().unwrap_or("the last result")),
        })];
        parts.extend(
            m.pictures
                .iter()
                .map(|p| json!({ "type": "image_url", "image_url": { "url": p.data_url() } })),
        );
        out.push(json!({ "role": "user", "content": parts }));
    }
    out
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

/// The input window of an OpenAI model by its family, for OpenAI's own API. Unknown names
/// give `None`, and the harness keeps its smallest budget for them.
fn openai_window(model: &str) -> Option<u32> {
    let m = model.to_ascii_lowercase();
    let w = if m.starts_with("gpt-5") {
        272_000
    } else if m.starts_with("gpt-4.1") {
        1_000_000
    } else if m.starts_with("gpt-4o") || m.starts_with("gpt-4-turbo") {
        128_000
    } else if m.starts_with("o1") || m.starts_with("o3") || m.starts_with("o4") {
        200_000
    } else {
        return None;
    };
    Some(w)
}

#[async_trait]
impl Provider for OpenAi {
    fn reads_pictures(&self) -> bool {
        self.pictures
    }
    fn context_window(&self) -> Option<u32> {
        self.window
    }
    fn describe(&self) -> String {
        format!("{} {}", self.name, self.model)
    }
    fn model_id(&self) -> String {
        // A named endpoint's id is a spec (`router:claude`), so its account and capacity are
        // the endpoint's, never a built-in provider whose name equals the model's.
        if self.name == "openai" {
            self.model.clone()
        } else {
            format!("{}:{}", self.name, self.model)
        }
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
            return Err(status_error(&self.name, resp, error_detail).await);
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
                    &self.name,
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
            return Err(status_error(&self.name, resp, error_detail).await);
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
        wire.extend(messages.iter().flat_map(to_wire));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Picture;

    #[test]
    fn a_tool_results_picture_follows_it_in_a_user_message() {
        let call = ToolCall {
            id: "t1".into(),
            name: "app.see".into(),
            arguments: json!({}),
        };
        let pic = Picture {
            media_type: "image/png".into(),
            bytes: std::sync::Arc::new(b"png".to_vec()),
        };
        let w = to_wire(&Message::tool_result(&call, "a window").with_pictures(vec![pic]));
        assert_eq!(w.len(), 2);
        assert_eq!(w[0]["role"], "tool");
        assert_eq!(w[0]["content"], "a window");
        assert_eq!(w[1]["role"], "user");
        assert_eq!(
            w[1]["content"][1]["image_url"]["url"],
            "data:image/png;base64,cG5n"
        );
        // Without a picture, one message as before.
        assert_eq!(to_wire(&Message::tool_result(&call, "x")).len(), 1);
    }

    #[test]
    fn openai_models_have_their_family_window() {
        assert_eq!(openai_window("gpt-5"), Some(272_000));
        assert_eq!(openai_window("gpt-5-mini"), Some(272_000));
        assert_eq!(openai_window("gpt-4.1-mini"), Some(1_000_000));
        assert_eq!(openai_window("gpt-4o"), Some(128_000));
        assert_eq!(openai_window("o3"), Some(200_000));
        assert_eq!(openai_window("some-local-model"), None);
    }
}
