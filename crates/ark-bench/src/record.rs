//! Recording what each model request contained and cost. [`Metered`] wraps any provider and
//! appends one [`RequestRecord`] per request to a shared [`Recorder`]: the estimated size of
//! each part (system, tool schemas, messages), the provider's measured usage, latency, time to
//! first token, and how much of the request repeated an earlier one byte for byte (the part a
//! prefix cache could serve).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use ark_sdk::prelude::ark_model::{
    estimate_tokens, Completion, Message, Provider, Result as ModelResult, TextSink, ToolDef,
    Usage, WebHit,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// One model request as the benchmark saw it.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RequestRecord {
    pub model: String,
    /// Tools exposed on this request.
    pub tools: u32,
    pub tool_schema_tokens: u32,
    pub system_tokens: u32,
    pub message_tokens: u32,
    pub messages: u32,
    /// system + messages + tool schemas, estimated.
    pub est_prompt_tokens: u32,
    /// Estimated tokens at the start of this request identical to one of the last few
    /// requests, in provider render order (tools, system, messages).
    pub prefix_reuse_tokens: u32,
    pub usage: Usage,
    pub latency_ms: u64,
    pub first_token_ms: Option<u64>,
    pub error: Option<String>,
    /// Tool names the model asked for in its reply.
    pub calls: Vec<String>,
    /// Tokens the provider reports writing to its prompt cache (when it reports them).
    #[serde(default)]
    pub cache_write_tokens: u32,
}

/// Tracks recent request renderings to measure prefix reuse.
#[derive(Default)]
pub struct PrefixTracker {
    recent: VecDeque<String>,
}

impl PrefixTracker {
    const KEEP: usize = 6;

    /// Longest common prefix, in estimated tokens, between `rendered` and any recent request.
    /// Then remember `rendered`.
    pub fn observe(&mut self, rendered: String) -> u32 {
        let best = self
            .recent
            .iter()
            .map(|r| common_prefix_chars(r, &rendered))
            .max()
            .unwrap_or(0);
        self.recent.push_back(rendered);
        if self.recent.len() > Self::KEEP {
            self.recent.pop_front();
        }
        (best as f32 / 4.0).floor() as u32
    }
}

fn common_prefix_chars(a: &str, b: &str) -> usize {
    a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count()
}

/// The request in the order providers render it (tools, then system, then messages), as one
/// string. Only used to compare requests with each other.
pub fn render(system: &str, messages: &[Message], tools: &[ToolDef]) -> String {
    let mut s = String::new();
    for t in tools {
        s.push_str(&t.name);
        s.push('\u{0}');
        s.push_str(&t.description);
        s.push('\u{0}');
        s.push_str(&t.parameters.to_string());
        s.push('\u{1}');
    }
    s.push('\u{2}');
    s.push_str(system);
    for m in messages {
        s.push('\u{3}');
        s.push_str(&format!("{:?}", m.role));
        s.push('\u{0}');
        s.push_str(&m.content);
        for c in &m.tool_calls {
            s.push('\u{0}');
            s.push_str(&c.name);
            s.push_str(&c.arguments.to_string());
        }
        if let Some(id) = &m.tool_call_id {
            s.push('\u{0}');
            s.push_str(id);
        }
    }
    s
}

pub fn tool_schema_tokens(tools: &[ToolDef]) -> u32 {
    tools
        .iter()
        .map(|t| {
            estimate_tokens(&t.name)
                + estimate_tokens(&t.description)
                + estimate_tokens(&t.parameters.to_string())
        })
        .sum()
}

pub fn message_tokens(messages: &[Message]) -> u32 {
    messages
        .iter()
        .map(|m| {
            let mut t = estimate_tokens(&m.content) + 4;
            for c in &m.tool_calls {
                t += estimate_tokens(&c.name) + estimate_tokens(&c.arguments.to_string()) + 8;
            }
            t
        })
        .sum()
}

/// The shared log every [`Metered`] provider of one run writes to.
#[derive(Default)]
pub struct Recorder {
    records: Mutex<Vec<RequestRecord>>,
    prefix: Mutex<PrefixTracker>,
    sim_writes: Mutex<u64>,
    faults: Mutex<u32>,
}

impl Recorder {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
    /// Tokens the simulated model wrote to its cache.
    pub fn add_sim_write(&self, tokens: u32) {
        *self.sim_writes.lock().unwrap() += tokens as u64;
    }
    /// Cache writes over the run: simulated ones plus any a provider reported.
    pub fn cache_writes(&self) -> u64 {
        *self.sim_writes.lock().unwrap()
            + self
                .records
                .lock()
                .unwrap()
                .iter()
                .map(|r| r.cache_write_tokens as u64)
                .sum::<u64>()
    }
    pub(crate) fn simulated_cache_writes(&self) -> u64 {
        *self.sim_writes.lock().unwrap()
    }
    /// A fault was injected into one of the run's model calls.
    pub fn add_fault(&self) {
        *self.faults.lock().unwrap() += 1;
    }
    pub fn faults(&self) -> u32 {
        *self.faults.lock().unwrap()
    }
    pub fn records(&self) -> Vec<RequestRecord> {
        self.records.lock().unwrap().clone()
    }
    pub fn len(&self) -> usize {
        self.records.lock().unwrap().len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A provider wrapper that records every request into a [`Recorder`].
pub struct Metered {
    inner: Arc<dyn Provider>,
    recorder: Arc<Recorder>,
}

impl Metered {
    pub fn wrap(inner: Arc<dyn Provider>, recorder: Arc<Recorder>) -> Arc<dyn Provider> {
        Arc::new(Metered { inner, recorder })
    }

    fn start(&self, system: &str, messages: &[Message], tools: &[ToolDef]) -> RequestRecord {
        let tool_schema = tool_schema_tokens(tools);
        let system_tokens = estimate_tokens(system);
        let msg = message_tokens(messages);
        let reuse = self
            .recorder
            .prefix
            .lock()
            .unwrap()
            .observe(render(system, messages, tools));
        RequestRecord {
            model: self.inner.model_id(),
            tools: tools.len() as u32,
            tool_schema_tokens: tool_schema,
            system_tokens,
            message_tokens: msg,
            messages: messages.len() as u32,
            est_prompt_tokens: system_tokens + msg + tool_schema,
            prefix_reuse_tokens: reuse,
            ..Default::default()
        }
    }

    fn finish(&self, mut rec: RequestRecord, started: Instant, r: &ModelResult<Completion>) {
        rec.latency_ms = started.elapsed().as_millis() as u64;
        match r {
            Ok(c) => {
                rec.usage = c.usage;
                // Read by name, so this builds against runtimes without the field.
                rec.cache_write_tokens = serde_json::to_value(c.usage)
                    .ok()
                    .and_then(|v| v.get("cache_write_tokens").and_then(|w| w.as_u64()))
                    .unwrap_or(0) as u32;
                rec.calls = c.tool_calls.iter().map(|t| t.name.clone()).collect();
            }
            Err(e) => rec.error = Some(e.to_string()),
        }
        self.recorder.records.lock().unwrap().push(rec);
    }
}

#[async_trait]
impl Provider for Metered {
    fn for_conversation(&self, conversation: &str) -> ModelResult<Option<Arc<dyn Provider>>> {
        Ok(self
            .inner
            .for_conversation(conversation)?
            .map(|inner| Self::wrap(inner, self.recorder.clone())))
    }

    async fn complete(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> ModelResult<Completion> {
        let rec = self.start(system, messages, tools);
        let started = Instant::now();
        let r = self.inner.complete(system, messages, tools).await;
        self.finish(rec, started, &r);
        r
    }

    async fn complete_streaming(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDef],
        on_text: TextSink<'_>,
    ) -> ModelResult<Completion> {
        let mut rec = self.start(system, messages, tools);
        let started = Instant::now();
        let first: Mutex<Option<u64>> = Mutex::new(None);
        let sink = |t: &str| {
            first
                .lock()
                .unwrap()
                .get_or_insert(started.elapsed().as_millis() as u64);
            on_text(t)
        };
        let r = self
            .inner
            .complete_streaming(system, messages, tools, &sink)
            .await;
        rec.first_token_ms = *first.lock().unwrap();
        self.finish(rec, started, &r);
        r
    }

    /// Searches go through unchanged; they are not model calls of the run.
    async fn web_search(&self, query: &str, limit: usize) -> Option<ModelResult<Vec<WebHit>>> {
        self.inner.web_search(query, limit).await
    }

    fn describe(&self) -> String {
        self.inner.describe()
    }
    fn model_id(&self) -> String {
        self.inner.model_id()
    }
    fn context_window(&self) -> Option<u32> {
        self.inner.context_window()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_reuse_counts_the_shared_start() {
        let mut t = PrefixTracker::default();
        assert_eq!(t.observe("a".repeat(400)), 0);
        // The first 400 characters match: 100 tokens.
        assert_eq!(
            t.observe(format!("{}{}", "a".repeat(400), "b".repeat(40))),
            100
        );
        assert_eq!(t.observe("c".repeat(400)), 0);
    }
}
