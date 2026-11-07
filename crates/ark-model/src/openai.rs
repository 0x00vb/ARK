//! OpenAI-compatible chat completions (also serves any OpenAI-compatible endpoint via
//! `OPENAI_BASE_URL`). API-key auth.

use crate::resilience::{raw_status_error, status_error, HttpConfig};
use crate::stream::{for_each_line, parse_args, sse_data, OpenAiFold};
use crate::{
    mangle_tool_name, unmangle_tool_name, Completion, GeneratedImage, Message, Provider,
    ProviderError, Result, Role, TextSink, ToolCall, ToolDef,
};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{json, Value};

const MAX_GENERATED_IMAGE_BYTES: usize = 25 * 1024 * 1024;
const MAX_IMAGE_RESPONSE_BYTES: usize = 36 * 1024 * 1024;

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
    headers: reqwest::header::HeaderMap,
    raw_status: bool,
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
            headers: reqwest::header::HeaderMap::new(),
            raw_status: false,
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

    pub(crate) fn with_raw_status(mut self) -> Self {
        self.raw_status = true;
        self
    }

    async fn response_error(&self, response: reqwest::Response) -> ProviderError {
        if self.raw_status {
            raw_status_error(&self.name, response, error_detail, Some(&self.key)).await
        } else {
            status_error(&self.name, response, error_detail).await
        }
    }

    pub(crate) fn with_transport(
        mut self,
        client: reqwest::Client,
        headers: reqwest::header::HeaderMap,
        http: HttpConfig,
    ) -> Self {
        self.client = client;
        self.headers = headers;
        self.http = http;
        self
    }
}

fn error_detail(v: &Value) -> Option<String> {
    v["error"]["message"]
        .as_str()
        .or_else(|| v["message"].as_str())
        .map(str::to_string)
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
    // A person's message with images is one message: its text, then the images.
    if m.role == Role::User && !m.pictures.is_empty() {
        let mut parts = vec![json!({ "type": "text", "text": m.content })];
        parts.extend(
            m.pictures
                .iter()
                .map(|p| json!({ "type": "image_url", "image_url": { "url": p.data_url() } })),
        );
        return vec![json!({ "role": "user", "content": parts })];
    }
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
    fn host_controls_tools(&self) -> bool {
        true
    }
    fn generates_images(&self) -> bool {
        self.name == "openai" && supports_image_tool(&self.model)
    }

    async fn generate_image(&self, prompt: &str) -> Option<Result<GeneratedImage>> {
        if !self.generates_images() {
            return None;
        }
        Some(self.generate_image_request(prompt).await)
    }
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
            return Err(self.response_error(resp).await);
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
            return Err(self.response_error(resp).await);
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
    async fn generate_image_request(&self, prompt: &str) -> Result<GeneratedImage> {
        use base64::Engine as _;
        let model =
            std::env::var("ARK_IMAGE_MODEL").unwrap_or_else(|_| "gpt-image-2.5-flare".into());
        let body = json!({
            "model": model,
            "prompt": prompt,
            "n": 1,
            "response_format": "b64_json",
            "output_format": "png"
        });
        let resp = self
            .client
            .post(format!("{}/images/generations", self.base))
            .bearer_auth(&self.key)
            .json(&body)
            .timeout(self.http.request_timeout)
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(self.response_error(resp).await);
        }
        if resp
            .content_length()
            .is_some_and(|length| length > MAX_IMAGE_RESPONSE_BYTES as u64)
        {
            return Err(image_response_too_large(&self.name));
        }
        let mut body = Vec::new();
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            if body.len().saturating_add(chunk.len()) > MAX_IMAGE_RESPONSE_BYTES {
                return Err(image_response_too_large(&self.name));
            }
            body.extend_from_slice(&chunk);
        }
        let value: Value = serde_json::from_slice(&body)?;
        let encoded = value["data"][0]["b64_json"]
            .as_str()
            .ok_or_else(|| ProviderError::Api {
                provider: self.name.clone(),
                detail: "image generation returned no image bytes".into(),
            })?;
        let max_encoded = MAX_GENERATED_IMAGE_BYTES.div_ceil(3) * 4;
        if encoded.len() > max_encoded {
            return Err(image_response_too_large(&self.name));
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| ProviderError::Api {
                provider: self.name.clone(),
                detail: "image generation returned invalid base64".into(),
            })?;
        Ok(GeneratedImage {
            media_type: "image/png".into(),
            bytes,
        })
    }

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
            .headers(self.headers.clone())
            .bearer_auth(&self.key)
            .json(body);
        if !stream {
            req = req.timeout(self.http.request_timeout);
        }
        Ok(req.send().await?)
    }
}

fn image_response_too_large(provider: &str) -> ProviderError {
    ProviderError::Api {
        provider: provider.into(),
        detail: "image generation response exceeds the 25 MB image limit".into(),
    }
}

fn supports_image_tool(model: &str) -> bool {
    let Some(rest) = model
        .to_ascii_lowercase()
        .strip_prefix("gpt-")
        .map(str::to_string)
    else {
        return false;
    };
    rest.split(['.', '-'])
        .next()
        .and_then(|n| n.parse::<u32>().ok())
        .is_some_and(|major| major >= 5)
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
    fn a_persons_message_is_one_message_with_its_images() {
        let pic = Picture {
            media_type: "image/png".into(),
            bytes: std::sync::Arc::new(b"png".to_vec()),
        };
        let w = to_wire(&Message::user("what is this?").with_pictures(vec![pic]));
        assert_eq!(w.len(), 1);
        assert_eq!(w[0]["role"], "user");
        assert_eq!(w[0]["content"][0]["text"], "what is this?");
        assert_eq!(
            w[0]["content"][1]["image_url"]["url"],
            "data:image/png;base64,cG5n"
        );
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

    #[test]
    fn only_supported_mainline_openai_models_offer_image_generation() {
        assert!(supports_image_tool("gpt-5"));
        assert!(supports_image_tool("gpt-6.1"));
        assert!(!supports_image_tool("gpt-4o"));
        assert!(!supports_image_tool("o3"));
        assert!(!supports_image_tool("gpt-image-2.5-flare"));
    }
}
