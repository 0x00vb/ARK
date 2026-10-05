//! Anthropic Messages API. API-key auth. Content is block-structured: assistant tool
//! requests are `tool_use` blocks and results are `tool_result` blocks.

use crate::resilience::{status_error, HttpConfig};
use crate::stream::{for_each_line, sse_data, AnthropicFold};
use crate::{
    mangle_tool_name, unmangle_tool_name, Completion, Message, Provider, ProviderError, Result,
    Role, TextSink, ToolCall, ToolDef, WebHit,
};
use async_trait::async_trait;
use serde_json::{json, Value};

/// Output cap when none is configured. Room for adaptive thinking plus an answer.
const DEFAULT_MAX_TOKENS: u32 = 8192;

pub struct Anthropic {
    base: String,
    model: String,
    key: String,
    client: reqwest::Client,
    http: HttpConfig,
}

impl Anthropic {
    pub fn from_env(model: impl Into<String>) -> Result<Self> {
        let key = crate::keys::api_key("ANTHROPIC_API_KEY")
            .ok_or_else(|| ProviderError::MissingCredential("ANTHROPIC_API_KEY".into()))?;
        let mut a = Anthropic::new(model, key);
        if let Ok(base) = std::env::var("ANTHROPIC_BASE_URL") {
            a.base = base;
        }
        Ok(a)
    }

    /// Construct explicitly (for consumers wiring their own credentials).
    pub fn new(model: impl Into<String>, key: impl Into<String>) -> Self {
        let http = HttpConfig::default();
        Anthropic {
            base: "https://api.anthropic.com/v1".into(),
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

    /// Send requests to another base URL (a proxy, or a test server).
    pub fn with_base_url(mut self, base: impl Into<String>) -> Self {
        self.base = base.into();
        self
    }
}

fn error_detail(v: &Value) -> Option<String> {
    v["error"]["message"].as_str().map(str::to_string)
}

/// One message as Anthropic content blocks. Anthropic has no `tool` role: results go in a
/// `user` message as `tool_result` blocks, and tool requests are `tool_use` blocks in an
/// assistant message. Empty text is dropped (the API rejects empty text blocks).
fn blocks(m: &Message) -> (&'static str, Vec<Value>) {
    match m.role {
        Role::Tool => (
            "user",
            vec![
                json!({ "type": "tool_result", "tool_use_id": m.tool_call_id.clone().unwrap_or_default(), "content": m.content }),
            ],
        ),
        Role::Assistant => {
            let mut b = Vec::new();
            if !m.content.is_empty() {
                b.push(json!({ "type": "text", "text": m.content }));
            }
            for c in &m.tool_calls {
                b.push(json!({ "type": "tool_use", "id": c.id, "name": mangle_tool_name(&c.name), "input": c.arguments }));
            }
            ("assistant", b)
        }
        // System is passed as a top-level field, not a message; anything else is a user turn.
        _ if m.content.is_empty() => ("user", Vec::new()),
        _ => ("user", vec![json!({ "type": "text", "text": m.content })]),
    }
}

/// The wire messages: consecutive messages of one role merged into one message (all the
/// results of parallel tool calls travel together, as the API expects), and a cache
/// breakpoint on the last block before the final one. The runtime ends every request with
/// the part that changes (the working state), so the breakpoint marks the end of what the
/// next request will repeat.
pub(crate) fn wire_messages(messages: &[Message]) -> Vec<Value> {
    let mut merged: Vec<(&'static str, Vec<Value>)> = Vec::new();
    for m in messages {
        let (role, b) = blocks(m);
        if b.is_empty() {
            continue;
        }
        match merged.last_mut() {
            Some((r, existing)) if *r == role => existing.extend(b),
            _ => merged.push((role, b)),
        }
    }
    let total: usize = merged.iter().map(|(_, b)| b.len()).sum();
    if total >= 2 {
        let mut seen = 0;
        'outer: for (_, b) in merged.iter_mut() {
            for block in b.iter_mut() {
                seen += 1;
                if seen == total - 1 {
                    block["cache_control"] = json!({ "type": "ephemeral" });
                    break 'outer;
                }
            }
        }
    }
    merged
        .into_iter()
        .map(|(role, content)| json!({ "role": role, "content": content }))
        .collect()
}

/// The pages a `web_search` reply found, in its order, with the first sentence the model
/// cited from each as its snippet.
fn search_hits(reply: &Value, limit: usize) -> Vec<WebHit> {
    let blocks = reply["content"].as_array().cloned().unwrap_or_default();
    let mut hits: Vec<WebHit> = Vec::new();
    for b in &blocks {
        if b["type"] != "web_search_tool_result" {
            continue;
        }
        for r in b["content"].as_array().into_iter().flatten() {
            let url = r["url"].as_str().unwrap_or_default();
            if url.is_empty() || hits.iter().any(|h| h.url == url) {
                continue;
            }
            hits.push(WebHit {
                title: r["title"].as_str().unwrap_or_default().to_string(),
                url: url.to_string(),
                snippet: String::new(),
            });
        }
    }
    for c in blocks
        .iter()
        .flat_map(|b| b["citations"].as_array().cloned().unwrap_or_default())
    {
        let (Some(url), Some(text)) = (c["url"].as_str(), c["cited_text"].as_str()) else {
            continue;
        };
        if let Some(h) = hits
            .iter_mut()
            .find(|h| h.url == url && h.snippet.is_empty())
        {
            h.snippet = text.trim().to_string();
        }
    }
    hits.truncate(limit);
    hits
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
        let resp = self.post(&body, false).await?;
        if !resp.status().is_success() {
            return Err(status_error("anthropic", resp, error_detail).await);
        }
        let v: Value = resp.json().await?;
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
        if v["stop_reason"] == "max_tokens" && !tool_calls.is_empty() {
            // A tool call cut off at the output cap: its arguments may be incomplete.
            return Err(ProviderError::Malformed {
                provider: "anthropic".into(),
                detail: "the reply hit the output cap inside a tool call".into(),
            });
        }
        let usage = crate::stream::anthropic_usage(&v["usage"], None);
        Ok(Completion {
            content,
            tool_calls,
            usage,
        })
    }

    /// Anthropic's server-side `web_search` tool, used once. The results come back as
    /// `web_search_result` blocks; a sentence the model cited from a page becomes its snippet.
    async fn web_search(&self, query: &str, limit: usize) -> Option<Result<Vec<WebHit>>> {
        let body = json!({
            "model": self.model,
            "max_tokens": 1024,
            "messages": [{ "role": "user", "content": crate::search_prompt(query, limit) }],
            "tools": [{ "type": "web_search_20250305", "name": "web_search", "max_uses": 1 }],
        });
        let run = async {
            let resp = self.post(&body, false).await?;
            if !resp.status().is_success() {
                return Err(status_error("anthropic", resp, error_detail).await);
            }
            Ok(search_hits(&resp.json::<Value>().await?, limit))
        };
        Some(run.await)
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
            return Err(status_error("anthropic", resp, error_detail).await);
        }
        let mut fold = AnthropicFold::default();
        for_each_line(resp, |line| match sse_data(line) {
            Some(data) => fold.feed(&serde_json::from_str::<Value>(data)?, on_text),
            None => Ok(()),
        })
        .await?;
        fold.finish()
    }
}

impl Anthropic {
    fn body(&self, system: &str, messages: &[Message], tools: &[ToolDef], stream: bool) -> Value {
        // Prompt caching: breakpoints after the tool definitions, after the system prompt, and
        // before the last message block. The runtime keeps the first two fixed for a turn.
        // Prefixes shorter than the model's minimum are simply not cached.
        let mut tool_defs: Vec<Value> = tools
            .iter()
            .map(|t| json!({ "name": mangle_tool_name(&t.name), "description": t.description, "input_schema": t.parameters }))
            .collect();
        if let Some(last) = tool_defs.last_mut() {
            last["cache_control"] = json!({ "type": "ephemeral" });
        }
        json!({
            "model": self.model,
            "max_tokens": self.http.max_output_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
            "system": [{ "type": "text", "text": system, "cache_control": { "type": "ephemeral" } }],
            "messages": wire_messages(messages),
            "tools": tool_defs,
            "stream": stream,
        })
    }

    async fn post(&self, body: &Value, stream: bool) -> Result<reqwest::Response> {
        let mut req = self
            .client
            .post(format!("{}/messages", self.base))
            .header("x-api-key", &self.key)
            .header("anthropic-version", "2023-06-01")
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
    use crate::ToolCall;

    fn call(id: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: "fs.read".into(),
            arguments: json!({ "path": id }),
        }
    }

    #[test]
    fn parallel_results_travel_in_one_user_message() {
        let msgs = vec![
            Message::user("task"),
            Message::assistant_tool_calls(vec![call("a"), call("b")]),
            Message::tool_result(&call("a"), "ra"),
            Message::tool_result(&call("b"), "rb"),
            Message::user("== WORKING STATE =="),
        ];
        let w = wire_messages(&msgs);
        assert_eq!(w.len(), 3, "user, assistant, user: {w:#?}");
        let last = w[2]["content"].as_array().unwrap();
        assert_eq!(last.len(), 3, "two results and the state, merged");
        assert_eq!(last[0]["type"], "tool_result");
        assert_eq!(last[1]["type"], "tool_result");
        assert_eq!(last[2]["type"], "text");
    }

    #[test]
    fn breakpoint_sits_before_the_final_block() {
        let msgs = vec![
            Message::user("task"),
            Message::assistant_tool_calls(vec![call("a")]),
            Message::tool_result(&call("a"), "ra"),
            Message::user("state"),
        ];
        let w = wire_messages(&msgs);
        let marked: Vec<String> = w
            .iter()
            .flat_map(|m| m["content"].as_array().unwrap().iter())
            .filter(|b| b.get("cache_control").is_some())
            .map(|b| b["type"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            marked,
            vec!["tool_result"],
            "the last result, not the state"
        );
    }

    // The block shapes of a Messages API reply with the `web_search` server tool.
    #[test]
    fn search_results_come_from_result_blocks_with_cited_text_as_snippets() {
        let reply = json!({ "content": [
            { "type": "server_tool_use", "id": "s1", "name": "web_search", "input": { "query": "tea" } },
            { "type": "web_search_tool_result", "tool_use_id": "s1", "content": [
                { "type": "web_search_result", "url": "https://a.example/", "title": "A", "encrypted_content": "x" },
                { "type": "web_search_result", "url": "https://b.example/", "title": "B", "encrypted_content": "y" },
                { "type": "web_search_result", "url": "https://a.example/", "title": "A again", "encrypted_content": "z" }
            ]},
            { "type": "text", "text": "Brew at 80 C.", "citations": [
                { "type": "web_search_result_location", "url": "https://b.example/", "title": "B", "cited_text": " Brew green tea at 80 C. " }
            ]}
        ]});
        let hits = search_hits(&reply, 5);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].url, "https://a.example/");
        assert_eq!(hits[0].snippet, "");
        assert_eq!(hits[1].snippet, "Brew green tea at 80 C.");
        assert_eq!(search_hits(&reply, 1).len(), 1);
    }

    #[test]
    fn body_marks_tools_and_system_and_drops_empty_text() {
        let a = Anthropic::new("m", "k");
        let tools = vec![
            ToolDef {
                name: "a.b".into(),
                description: "x".into(),
                parameters: json!({}),
            },
            ToolDef {
                name: "c.d".into(),
                description: "y".into(),
                parameters: json!({}),
            },
        ];
        let msgs = vec![
            Message::user("task"),
            Message::assistant(""),
            Message::user("state"),
        ];
        let b = a.body("sys", &msgs, &tools, false);
        assert!(b["tools"][0].get("cache_control").is_none());
        assert_eq!(b["tools"][1]["cache_control"]["type"], "ephemeral");
        assert_eq!(b["system"][0]["cache_control"]["type"], "ephemeral");
        assert_eq!(b["system"][0]["text"], "sys");
        let m = b["messages"].as_array().unwrap();
        assert_eq!(
            m.len(),
            1,
            "the empty assistant turn is dropped and the users merge"
        );
        assert_eq!(m[0]["content"][0]["cache_control"]["type"], "ephemeral");
    }
}
