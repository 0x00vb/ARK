//! A scriptable in-memory provider for testing agent/runtime logic without a live model.
//! It returns queued completions in order and records every request, so a test can assert
//! on what the runtime sent (system prompt size, tool count, the message transcript) and
//! drive a deterministic multi-step loop.

use crate::{Completion, Message, Provider, Result, TextSink, ToolDef, Usage};
use async_trait::async_trait;
use std::sync::Mutex;

/// One recorded request the runtime made to the model.
#[derive(Clone, Debug)]
pub struct Recorded {
    pub system: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDef>,
}

/// Builds a reply from a request once the script is exhausted.
type Fallback = Box<dyn Fn(&Recorded) -> Completion + Send + Sync>;

/// A provider that replays a fixed script of completions and records requests.
pub struct MockProvider {
    id: String,
    script: Mutex<std::collections::VecDeque<Completion>>,
    pub calls: Mutex<Vec<Recorded>>,
    /// Pause between streamed chunks, so a demo or a cancellation test sees text arrive over time.
    chunk_delay: std::time::Duration,
    /// Fallback when the script runs out: a closure that builds a reply from the request.
    fallback: Option<Fallback>,
    /// The window it reports ([`Provider::context_window`]); unknown by default.
    window: Option<u32>,
}

impl MockProvider {
    pub fn new(id: impl Into<String>, script: Vec<Completion>) -> Self {
        MockProvider {
            id: id.into(),
            script: Mutex::new(script.into_iter().collect()),
            calls: Mutex::new(Vec::new()),
            chunk_delay: std::time::Duration::ZERO,
            fallback: None,
            window: None,
        }
    }

    /// A scripted provider from a JSON file: `[{"content": "…", "tool_calls": [{"name": "fs.read",
    /// "arguments": {…}}]}, …]`. Used by `ARK_PROVIDER=mock` with `ARK_MOCK_SCRIPT=<file>` so a
    /// front-end can be exercised end to end, in CI, without a live model. After the script, it
    /// answers with a short placeholder. `ARK_MOCK_DELAY_MS` paces the streamed words.
    pub fn from_script_file(path: &std::path::Path) -> Result<Self> {
        #[derive(serde::Deserialize)]
        struct Step {
            #[serde(default)]
            content: String,
            #[serde(default)]
            tool_calls: Vec<Call>,
        }
        #[derive(serde::Deserialize)]
        struct Call {
            name: String,
            #[serde(default)]
            arguments: serde_json::Value,
        }
        let text = std::fs::read_to_string(path).map_err(|e| crate::ProviderError::Api {
            provider: "mock".into(),
            detail: format!("reading {}: {e}", path.display()),
        })?;
        let steps: Vec<Step> = serde_json::from_str(&text)?;
        let script = steps
            .into_iter()
            .enumerate()
            .map(|(i, s)| Completion {
                content: s.content,
                tool_calls: s
                    .tool_calls
                    .into_iter()
                    .enumerate()
                    .map(|(j, c)| crate::ToolCall {
                        id: format!("mock_{i}_{j}"),
                        name: c.name,
                        arguments: c.arguments,
                    })
                    .collect(),
                usage: Usage::default(),
            })
            .collect();
        let delay = std::env::var("ARK_MOCK_DELAY_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(15);
        Ok(MockProvider::new("mock-script", script)
            .with_chunk_delay(std::time::Duration::from_millis(delay))
            .with_fallback(|_| Completion {
                content: "(the mock script is finished)".into(),
                tool_calls: vec![],
                usage: Usage::default(),
            }))
    }

    /// Report this input window, as a real model with a known one would.
    pub fn with_context_window(mut self, tokens: u32) -> Self {
        self.window = Some(tokens);
        self
    }

    /// Stream each word with this pause between chunks.
    pub fn with_chunk_delay(mut self, delay: std::time::Duration) -> Self {
        self.chunk_delay = delay;
        self
    }

    /// Answer with `f(request)` once the script is exhausted, instead of a fixed placeholder.
    /// Lets a long-running test or demo generate replies without scripting every turn.
    pub fn with_fallback(
        mut self,
        f: impl Fn(&Recorded) -> Completion + Send + Sync + 'static,
    ) -> Self {
        self.fallback = Some(Box::new(f));
        self
    }

    /// The number of times the model was called.
    pub fn call_count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }

    /// The recorded requests, in order.
    pub fn recorded(&self) -> Vec<Recorded> {
        self.calls.lock().unwrap().clone()
    }

    /// The largest prompt (system + all message content) the runtime ever sent, in
    /// estimated tokens — the headline number for a context-budget test.
    pub fn peak_prompt_tokens(&self) -> u32 {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .map(|c| {
                let body: usize = c.messages.iter().map(|m| m.content.chars().count()).sum();
                crate::estimate_tokens(&c.system) + crate::estimate_tokens(&"x".repeat(body))
            })
            .max()
            .unwrap_or(0)
    }
}

#[async_trait]
impl Provider for MockProvider {
    fn context_window(&self) -> Option<u32> {
        self.window
    }
    fn describe(&self) -> String {
        format!("mock {}", self.id)
    }
    fn model_id(&self) -> String {
        self.id.clone()
    }
    async fn complete(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> Result<Completion> {
        Ok(self.next(system, messages, tools))
    }

    async fn complete_streaming(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDef],
        on_text: TextSink<'_>,
    ) -> Result<Completion> {
        let c = self.next(system, messages, tools);
        for chunk in c.content.split_inclusive(' ') {
            if !self.chunk_delay.is_zero() {
                tokio::time::sleep(self.chunk_delay).await;
            }
            on_text(chunk);
        }
        Ok(c)
    }
}

impl MockProvider {
    fn next(&self, system: &str, messages: &[Message], tools: &[ToolDef]) -> Completion {
        let rec = Recorded {
            system: system.to_string(),
            messages: messages.to_vec(),
            tools: tools.to_vec(),
        };
        let scripted = self.script.lock().unwrap().pop_front();
        let mut c = match (scripted, &self.fallback) {
            (Some(c), _) => c,
            (None, Some(f)) => f(&rec),
            (None, None) => Completion {
                content: "(mock exhausted)".into(),
                tool_calls: vec![],
                usage: Usage::default(),
            },
        };
        self.calls.lock().unwrap().push(rec);
        // Give a plausible measured usage if the script did not set one.
        if c.usage == Usage::default() {
            let prompt: usize = system.chars().count()
                + messages
                    .iter()
                    .map(|m| m.content.chars().count())
                    .sum::<usize>();
            c.usage = Usage::measured(
                crate::estimate_tokens(&"x".repeat(prompt)),
                crate::estimate_tokens(&c.content).max(1),
            );
        }
        c
    }
}
