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
    start_usage: Value,
    output_tokens: Option<u64>,
}

/// Anthropic reports uncached input, cache reads and cache writes separately; the prompt is
/// their sum. `output` overrides the output count (streaming sends it last).
pub(crate) fn anthropic_usage(u: &Value, output: Option<u64>) -> Usage {
    let n = |k: &str| u.get(k).and_then(|c| c.as_u64());
    match (n("input_tokens"), output.or_else(|| n("output_tokens"))) {
        (Some(input), Some(out)) => {
            let read = n("cache_read_input_tokens").unwrap_or(0);
            let write = n("cache_creation_input_tokens").unwrap_or(0);
            Usage::measured((input + read + write) as u32, out as u32)
                .with_cached(read as u32)
                .with_cache_write(write as u32)
        }
        _ => Usage::default(),
    }
}

/// OpenAI's `prompt_tokens` already includes the cached part.
pub(crate) fn openai_usage(u: &Value) -> Option<Usage> {
    let p = u.get("prompt_tokens")?.as_u64()?;
    let c = u.get("completion_tokens")?.as_u64()?;
    let cached = u["prompt_tokens_details"]["cached_tokens"]
        .as_u64()
        .unwrap_or(0);
    Some(Usage::measured(p as u32, c as u32).with_cached(cached as u32))
}

impl AnthropicFold {
    pub(crate) fn feed(&mut self, ev: &Value, on_text: TextSink<'_>) -> Result<()> {
        match ev["type"].as_str().unwrap_or_default() {
            "message_start" => {
                self.start_usage = ev["message"]["usage"].clone();
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
                let detail = ev["error"]["message"]
                    .as_str()
                    .unwrap_or("stream error")
                    .to_string();
                let provider = "anthropic".to_string();
                return Err(match ev["error"]["type"].as_str().unwrap_or_default() {
                    "overloaded_error" | "api_error" | "rate_limit_error" => {
                        ProviderError::Overloaded { provider, detail }
                    }
                    _ => ProviderError::Api { provider, detail },
                });
            }
            _ => {}
        }
        Ok(())
    }

    pub(crate) fn finish(self) -> Result<Completion> {
        let tool_calls = self
            .tools
            .into_iter()
            .map(|(_, id, name, json_text)| {
                Ok(ToolCall {
                    id,
                    arguments: parse_args("anthropic", &name, &json_text)?,
                    name: unmangle_tool_name(&name),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let usage = anthropic_usage(&self.start_usage, self.output_tokens);
        Ok(Completion {
            content: self.content,
            tool_calls,
            usage,
        })
    }
}

/// Folds OpenAI chat-completions streaming chunks into a completion.
#[derive(Default)]
pub(crate) struct OpenAiFold {
    content: String,
    /// Tool calls by index: (id, name, partial arguments).
    tools: Vec<(u64, String, String, String)>,
    usage: Option<Usage>,
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
        if let Some(u) = openai_usage(&chunk["usage"]) {
            self.usage = Some(u);
        }
        Ok(())
    }

    pub(crate) fn finish(self) -> Result<Completion> {
        let tool_calls = self
            .tools
            .into_iter()
            .map(|(i, id, name, args)| {
                Ok(ToolCall {
                    id: if id.is_empty() {
                        format!("call_{i}")
                    } else {
                        id
                    },
                    arguments: parse_args("openai", &name, &args)?,
                    name: unmangle_tool_name(&name),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let usage = self.usage.unwrap_or_default();
        Ok(Completion {
            content: self.content,
            tool_calls,
            usage,
        })
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
                    Some(Value::String(s)) => parse_args("ollama", name, s)?,
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

/// Tool-call arguments as JSON. Empty means no arguments; anything that does not parse (a
/// reply cut off mid-call) is an error, never silently `{}`.
pub(crate) fn parse_args(provider: &str, tool: &str, text: &str) -> Result<Value> {
    if text.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(text).map_err(|e| ProviderError::Malformed {
        provider: provider.to_string(),
        detail: format!("arguments for `{tool}` are not valid JSON ({e})"),
    })
}

/// Streams one top-level string field out of a JSON object that arrives in pieces: a
/// structured reply's `text` while the model is still writing it. Each [`push`] returns the
/// field's newly decoded characters. Escapes split across pieces are held until complete;
/// the same name inside a nested object or inside another string is ignored.
///
/// [`push`]: JsonTextField::push
pub(crate) struct JsonTextField {
    field: &'static str,
    depth: u32,
    in_string: bool,
    /// What the string being read is: a key at the top level, the field's value, or other.
    role: Role,
    key: String,
    /// At the top level, between a key and its value.
    after_colon: bool,
    last_key: String,
    /// The escape being read: `\` alone, or `\u` and up to four hex digits.
    escape: Option<String>,
    /// A high surrogate waiting for its low half.
    high: Option<u16>,
}

#[derive(Clone, Copy, PartialEq)]
enum Role {
    Key,
    Field,
    Other,
}

impl JsonTextField {
    pub(crate) fn new(field: &'static str) -> Self {
        JsonTextField {
            field,
            depth: 0,
            in_string: false,
            role: Role::Other,
            key: String::new(),
            after_colon: false,
            last_key: String::new(),
            escape: None,
            high: None,
        }
    }

    pub(crate) fn push(&mut self, piece: &str) -> String {
        let mut out = String::new();
        for c in piece.chars() {
            if self.in_string {
                self.string_char(c, &mut out);
                continue;
            }
            match c {
                '{' | '[' => self.depth += 1,
                '}' | ']' => self.depth = self.depth.saturating_sub(1),
                ':' if self.depth == 1 => self.after_colon = true,
                ',' if self.depth == 1 => self.after_colon = false,
                '"' => {
                    self.in_string = true;
                    self.role = if self.depth != 1 {
                        Role::Other
                    } else if !self.after_colon {
                        self.key.clear();
                        Role::Key
                    } else if self.last_key == self.field {
                        Role::Field
                    } else {
                        Role::Other
                    };
                }
                _ => {}
            }
        }
        out
    }

    fn string_char(&mut self, c: char, out: &mut String) {
        if let Some(esc) = self.escape.as_mut() {
            esc.push(c);
            let decoded = match esc.as_str() {
                e if e.starts_with("\\u") && e.len() < 6 => return,
                e if e.starts_with("\\u") => {
                    let unit = u16::from_str_radix(&e[2..], 16).unwrap_or(0xFFFD);
                    self.escape = None;
                    self.unit(unit, out);
                    return;
                }
                "\\n" => '\n',
                "\\t" => '\t',
                "\\r" => '\r',
                "\\b" => '\u{8}',
                "\\f" => '\u{c}',
                e => e.chars().nth(1).unwrap_or(c),
            };
            self.escape = None;
            self.emit(decoded, out);
            return;
        }
        match c {
            '\\' => self.escape = Some("\\".into()),
            '"' => {
                self.in_string = false;
                if self.role == Role::Key {
                    self.last_key = std::mem::take(&mut self.key);
                }
                self.role = Role::Other;
            }
            _ => self.emit(c, out),
        }
    }

    /// One UTF-16 code unit from a `\u` escape.
    fn unit(&mut self, unit: u16, out: &mut String) {
        if (0xD800..0xDC00).contains(&unit) {
            self.high = Some(unit);
            return;
        }
        let c = match self.high.take() {
            Some(high) if (0xDC00..0xE000).contains(&unit) => {
                char::decode_utf16([high, unit]).next().and_then(|r| r.ok())
            }
            _ => char::from_u32(unit as u32),
        };
        self.emit(c.unwrap_or('\u{FFFD}'), out);
    }

    fn emit(&mut self, c: char, out: &mut String) {
        match self.role {
            Role::Key => self.key.push(c),
            Role::Field => out.push(c),
            Role::Other => {}
        }
    }
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
        let c = f.finish().unwrap();
        assert_eq!(*seen.lock().unwrap(), "Let me check.");
        assert_eq!(c.content, "Let me check.");
        assert_eq!(c.tool_calls.len(), 1);
        assert_eq!(c.tool_calls[0].name, "fs.read");
        assert_eq!(c.tool_calls[0].arguments["path"], "a.md");
        assert_eq!(c.usage, Usage::measured(120, 17));
    }

    #[test]
    fn cached_tokens_are_part_of_the_prompt() {
        let a = anthropic_usage(
            &json!({"input_tokens": 20, "cache_read_input_tokens": 900, "cache_creation_input_tokens": 80}),
            Some(12),
        );
        assert_eq!(
            a,
            Usage::measured(1000, 12)
                .with_cached(900)
                .with_cache_write(80)
        );
        let o = openai_usage(
            &json!({"prompt_tokens": 1500, "completion_tokens": 9, "prompt_tokens_details": {"cached_tokens": 1024}}),
        );
        assert_eq!(o, Some(Usage::measured(1500, 9).with_cached(1024)));
        assert_eq!(openai_usage(&json!({})), None);
    }

    #[test]
    fn anthropic_fold_surfaces_stream_errors() {
        let mut f = AnthropicFold::default();
        let err = f
            .feed(
                &json!({"type":"error","error":{"type":"overloaded_error","message":"overloaded"}}),
                &|_: &str| {},
            )
            .unwrap_err();
        assert!(err.is_transient(), "overload is worth a retry");
        let err = f
            .feed(
                &json!({"type":"error","error":{"type":"invalid_request_error","message":"bad"}}),
                &|_: &str| {},
            )
            .unwrap_err();
        assert!(!err.is_transient());
    }

    #[test]
    fn a_cut_off_tool_call_is_an_error_not_empty_arguments() {
        let mut f = AnthropicFold::default();
        for e in [
            json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"t","name":"fs__write"}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\": \"a"}}),
        ] {
            f.feed(&e, &|_: &str| {}).unwrap();
        }
        let err = f.finish().unwrap_err();
        assert!(matches!(err, ProviderError::Malformed { .. }), "{err}");
        assert!(err.is_transient());
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
        let c = f.finish().unwrap();
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

    // The pieces are the ones Claude Code streamed for a structured reply (2026-10-03,
    // `claude -p --output-format stream-json --include-partial-messages`).
    #[test]
    fn a_reply_field_streams_from_partial_json() {
        let mut f = JsonTextField::new("text");
        let pieces = [
            "",
            "{\"action\": \"reply",
            "\", \"text\": \"Hello",
            ",",
            " how are you today",
            "?",
            "\", \"tool_calls\": []",
            "}",
        ];
        let out: String = pieces.iter().map(|p| f.push(p)).collect();
        assert_eq!(out, "Hello, how are you today?");
    }

    #[test]
    fn escapes_split_across_pieces_decode_once() {
        let mut f = JsonTextField::new("text");
        let mut out = String::new();
        for p in [
            "{\"text\":\"a\\",
            "nb \\\"q\\",
            "\" \\u00",
            "e9 \\ud83d",
            "\\ude00\"}",
        ] {
            out.push_str(&f.push(p));
        }
        assert_eq!(out, "a\nb \"q\" é 😀");
    }

    #[test]
    fn the_field_name_elsewhere_is_ignored() {
        let mut f = JsonTextField::new("text");
        let json = r#"{"action":"call_tools","tool_calls":[{"name":"x","arguments":"{\"text\":\"no\"}"}],"note":"text","text":"yes"}"#;
        let out: String = json.chars().map(|c| f.push(&c.to_string())).collect();
        assert_eq!(out, "yes");
    }
}
