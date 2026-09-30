//! Streaming plumbing shared by the providers: split a byte stream into lines, and fold each
//! vendor's incremental events back into one [`Completion`]. The folding is pure so it is
//! unit-tested against recorded event shapes without a network.

use futures::StreamExt;
use serde_json::{json, Value};

use crate::{unmangle_tool_name, Completion, ProviderError, Result, TextSink, ToolCall, Usage};

/// Read a response body line by line, calling `on_line` for each complete line (without the
/// trailing newline). Bytes are buffered until a newline so a UTF-8 sequence split across
/// network chunks is never broken.
pub(crate) async fn for_each_line(
    resp: reqwest::Response,
    mut on_line: impl FnMut(&str) -> Result<()>,
) -> Result<()> {
    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        buf.extend_from_slice(&chunk?);
        while let Some(pos) = buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = buf.drain(..=pos).collect();
            let text = String::from_utf8_lossy(&line);
            on_line(text.trim_end_matches(['\r', '\n']))?;
        }
    }
    if !buf.is_empty() {
        on_line(String::from_utf8_lossy(&buf).trim_end())?;
    }
    Ok(())
}

/// The JSON payload of a server-sent-events `data:` line, if this line carries one.
pub(crate) fn sse_data(line: &str) -> Option<&str> {
    line.strip_prefix("data:")
        .map(str::trim)
        .filter(|d| !d.is_empty() && *d != "[DONE]")
}

/// Folds Anthropic Messages streaming events into a completion.
#[derive(Default)]
pub(crate) struct AnthropicFold {
    content: String,
    /// Tool-use blocks by content index: (id, name, partial JSON).
    tools: Vec<(usize, String, String, String)>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
}

impl AnthropicFold {
    pub(crate) fn feed(&mut self, ev: &Value, on_text: TextSink<'_>) -> Result<()> {
        match ev["type"].as_str().unwrap_or_default() {
            "message_start" => {
                self.input_tokens = ev["message"]["usage"]["input_tokens"].as_u64();
            }
            "content_block_start" => {
                let block = &ev["content_block"];
                if block["type"] == "tool_use" {
                    let idx = ev["index"].as_u64().unwrap_or(0) as usize;
                    self.tools.push((
                        idx,
                        block["id"].as_str().unwrap_or("call_0").to_string(),
                        block["name"].as_str().unwrap_or_default().to_string(),
                        String::new(),
                    ));
                }
            }
            "content_block_delta" => {
                let d = &ev["delta"];
                match d["type"].as_str().unwrap_or_default() {
                    "text_delta" => {
                        let t = d["text"].as_str().unwrap_or_default();
                        if !t.is_empty() {
                            self.content.push_str(t);
                            on_text(t);
                        }
                    }
                    "input_json_delta" => {
                        let idx = ev["index"].as_u64().unwrap_or(0) as usize;
                        if let Some(tool) = self.tools.iter_mut().find(|t| t.0 == idx) {
                            tool.3
                                .push_str(d["partial_json"].as_str().unwrap_or_default());
                        }
                    }
                    _ => {}
                }
            }
            "message_delta" => {
                if let Some(o) = ev["usage"]["output_tokens"].as_u64() {
                    self.output_tokens = Some(o);
                }
            }
            "error" => {
                return Err(ProviderError::Api {
                    provider: "anthropic".into(),
                    detail: ev["error"]["message"]
                        .as_str()
                        .unwrap_or("stream error")
                        .to_string(),
                })
            }
            _ => {}
        }
        Ok(())
    }

    pub(crate) fn finish(self) -> Completion {
        let tool_calls = self
            .tools
            .into_iter()
            .map(|(_, id, name, json_text)| ToolCall {
                id,
                name: unmangle_tool_name(&name),
                arguments: parse_args(&json_text),
            })
            .collect();
        let usage = match (self.input_tokens, self.output_tokens) {
            (Some(p), Some(c)) => Usage::measured(p as u32, c as u32),
            _ => Usage::default(),
        };
        Completion {
            content: self.content,
            tool_calls,
            usage,
        }
    }
}

/// Folds OpenAI chat-completions streaming chunks into a completion.
#[derive(Default)]
pub(crate) struct OpenAiFold {
    content: String,
    /// Tool calls by index: (id, name, partial arguments).
    tools: Vec<(u64, String, String, String)>,
    usage: Option<(u64, u64)>,
}

impl OpenAiFold {
    pub(crate) fn feed(&mut self, chunk: &Value, on_text: TextSink<'_>) -> Result<()> {
        if let Some(err) = chunk.get("error") {
            return Err(ProviderError::Api {
                provider: "openai".into(),
                detail: err["message"]
                    .as_str()
                    .unwrap_or("stream error")
                    .to_string(),
            });
        }
        let delta = &chunk["choices"][0]["delta"];
        if let Some(t) = delta.get("content").and_then(|c| c.as_str()) {
            if !t.is_empty() {
                self.content.push_str(t);
                on_text(t);
            }
        }
        if let Some(calls) = delta.get("tool_calls").and_then(|c| c.as_array()) {
            for c in calls {
                let idx = c["index"].as_u64().unwrap_or(0);
                if !self.tools.iter().any(|t| t.0 == idx) {
                    self.tools
                        .push((idx, String::new(), String::new(), String::new()));
                }
                let tool = self
                    .tools
                    .iter_mut()
                    .find(|t| t.0 == idx)
                    .expect("inserted above");
                if let Some(id) = c["id"].as_str() {
                    tool.1 = id.to_string();
                }
                if let Some(name) = c["function"]["name"].as_str() {
                    tool.2.push_str(name);
                }
                if let Some(args) = c["function"]["arguments"].as_str() {
                    tool.3.push_str(args);
                }
            }
        }
        let u = &chunk["usage"];
        if let (Some(p), Some(c)) = (u["prompt_tokens"].as_u64(), u["completion_tokens"].as_u64()) {
            self.usage = Some((p, c));
        }
        Ok(())
    }

    pub(crate) fn finish(self) -> Completion {
        let tool_calls = self
            .tools
            .into_iter()
            .map(|(i, id, name, args)| ToolCall {
                id: if id.is_empty() {
                    format!("call_{i}")
                } else {
                    id
                },
                name: unmangle_tool_name(&name),
                arguments: parse_args(&args),
            })
            .collect();
        let usage = self
            .usage
            .map(|(p, c)| Usage::measured(p as u32, c as u32))
            .unwrap_or_default();
        Completion {
            content: self.content,
            tool_calls,
            usage,
        }
    }
}

/// Folds Ollama `/api/chat` NDJSON chunks into a completion.
#[derive(Default)]
pub(crate) struct OllamaFold {
    content: String,
    tool_calls: Vec<ToolCall>,
    usage: Option<(u64, u64)>,
}

impl OllamaFold {
    pub(crate) fn feed(&mut self, chunk: &Value, on_text: TextSink<'_>) -> Result<()> {
        if let Some(err) = chunk.get("error").and_then(|e| e.as_str()) {
            return Err(ProviderError::Api {
                provider: "ollama".into(),
                detail: err.to_string(),
            });
        }
        let msg = &chunk["message"];
        if let Some(t) = msg.get("content").and_then(|c| c.as_str()) {
            if !t.is_empty() {
                self.content.push_str(t);
                on_text(t);
            }
        }
        if let Some(calls) = msg.get("tool_calls").and_then(|c| c.as_array()) {
            for c in calls {
                let f = &c["function"];
                let name = f.get("name").and_then(|n| n.as_str()).unwrap_or_default();
                let arguments = match f.get("arguments") {
                    Some(Value::String(s)) => parse_args(s),
                    Some(other) => other.clone(),
                    None => json!({}),
                };
                let id = format!("call_{}", self.tool_calls.len());
                self.tool_calls.push(ToolCall {
                    id,
                    name: unmangle_tool_name(name),
                    arguments,
                });
            }
        }
        if chunk["done"].as_bool().unwrap_or(false) {
            if let (Some(p), Some(c)) = (
                chunk["prompt_eval_count"].as_u64(),
                chunk["eval_count"].as_u64(),
            ) {
                self.usage = Some((p, c));
            }
        }
        Ok(())
    }

    pub(crate) fn finish(self) -> Completion {
        let usage = self
            .usage
            .map(|(p, c)| Usage::measured(p as u32, c as u32))
            .unwrap_or_default();
        Completion {
            content: self.content,
            tool_calls: self.tool_calls,
            usage,
        }
    }
}

fn parse_args(text: &str) -> Value {
    if text.trim().is_empty() {
        return json!({});
    }
    serde_json::from_str(text).unwrap_or(json!({}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn collect() -> (Mutex<String>, impl Fn(&Mutex<String>, &str)) {
        (Mutex::new(String::new()), |m: &Mutex<String>, t: &str| {
            m.lock().unwrap().push_str(t)
        })
    }

    #[test]
    fn sse_data_skips_done_and_non_data() {
        assert_eq!(sse_data("data: {\"a\":1}"), Some("{\"a\":1}"));
        assert_eq!(sse_data("data: [DONE]"), None);
        assert_eq!(sse_data("event: message_start"), None);
        assert_eq!(sse_data(""), None);
    }

    #[test]
    fn anthropic_fold_streams_text_and_assembles_tool_use() {
        let (seen, push) = collect();
        let sink = |t: &str| push(&seen, t);
        let mut f = AnthropicFold::default();
        let events = [
            json!({"type":"message_start","message":{"usage":{"input_tokens":120}}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Let me "}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"check."}}),
            json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"tu_1","name":"fs__read"}}),
            json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"path\":"}}),
            json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"\"a.md\"}"}}),
            json!({"type":"message_delta","usage":{"output_tokens":17}}),
        ];
        for e in &events {
            f.feed(e, &sink).unwrap();
        }
        let c = f.finish();
        assert_eq!(*seen.lock().unwrap(), "Let me check.");
        assert_eq!(c.content, "Let me check.");
        assert_eq!(c.tool_calls.len(), 1);
        assert_eq!(c.tool_calls[0].name, "fs.read");
        assert_eq!(c.tool_calls[0].arguments["path"], "a.md");
        assert_eq!(c.usage, Usage::measured(120, 17));
    }

    #[test]
    fn anthropic_fold_surfaces_stream_errors() {
        let mut f = AnthropicFold::default();
        let err = f.feed(
            &json!({"type":"error","error":{"message":"overloaded"}}),
            &|_: &str| {},
        );
        assert!(err.is_err());
    }

    #[test]
    fn openai_fold_merges_tool_call_fragments() {
        let (seen, push) = collect();
        let sink = |t: &str| push(&seen, t);
        let mut f = OpenAiFold::default();
        let chunks = [
            json!({"choices":[{"delta":{"content":"Hi"}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","function":{"name":"calc__eval","arguments":"{\"ex"}}]}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"pr\":\"1+1\"}"}}]}}]}),
            json!({"choices":[],"usage":{"prompt_tokens":50,"completion_tokens":9}}),
        ];
        for c in &chunks {
            f.feed(c, &sink).unwrap();
        }
        let c = f.finish();
        assert_eq!(*seen.lock().unwrap(), "Hi");
        assert_eq!(c.tool_calls[0].id, "call_a");
        assert_eq!(c.tool_calls[0].name, "calc.eval");
        assert_eq!(c.tool_calls[0].arguments["expr"], "1+1");
        assert_eq!(c.usage, Usage::measured(50, 9));
    }

    #[test]
    fn ollama_fold_reads_final_counts() {
        let (seen, push) = collect();
        let sink = |t: &str| push(&seen, t);
        let mut f = OllamaFold::default();
        for c in [
            json!({"message":{"content":"The "},"done":false}),
            json!({"message":{"content":"answer."},"done":false}),
            json!({"message":{"content":"","tool_calls":[{"function":{"name":"clock__now","arguments":{}}}]},"done":false}),
            json!({"message":{"content":""},"done":true,"prompt_eval_count":80,"eval_count":6}),
        ] {
            f.feed(&c, &sink).unwrap();
        }
        let c = f.finish();
        assert_eq!(*seen.lock().unwrap(), "The answer.");
        assert_eq!(c.tool_calls[0].name, "clock.now");
        assert_eq!(c.usage, Usage::measured(80, 6));
    }
}
