//! A scriptable in-memory provider for testing agent/runtime logic without a live model.
//! It returns queued completions in order and records every request, so a test can assert
//! on what the runtime sent (system prompt size, tool count, the message transcript) and
//! drive a deterministic multi-step loop.

use crate::{Completion, Message, Provider, Result, ToolDef, Usage};
use async_trait::async_trait;
use std::sync::Mutex;

/// One recorded request the runtime made to the model.
#[derive(Clone, Debug)]
pub struct Recorded {
    pub system: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDef>,
}

/// A provider that replays a fixed script of completions and records requests.
pub struct MockProvider {
    id: String,
    script: Mutex<std::collections::VecDeque<Completion>>,
    pub calls: Mutex<Vec<Recorded>>,
}

impl MockProvider {
    pub fn new(id: impl Into<String>, script: Vec<Completion>) -> Self {
        MockProvider {
            id: id.into(),
            script: Mutex::new(script.into_iter().collect()),
            calls: Mutex::new(Vec::new()),
        }
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
    fn describe(&self) -> String {
        format!("mock {}", self.id)
    }
    fn model_id(&self) -> String {
        self.id.clone()
    }
    async fn complete(&self, system: &str, messages: &[Message], tools: &[ToolDef]) -> Result<Completion> {
        self.calls.lock().unwrap().push(Recorded {
            system: system.to_string(),
            messages: messages.to_vec(),
            tools: tools.to_vec(),
        });
        let mut c = self
            .script
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Completion { content: "(mock exhausted)".into(), tool_calls: vec![], usage: Usage::default() });
        // Give a plausible measured usage if the script did not set one.
        if c.usage == Usage::default() {
            let prompt: usize = system.chars().count() + messages.iter().map(|m| m.content.chars().count()).sum::<usize>();
            c.usage = Usage::measured(crate::estimate_tokens(&"x".repeat(prompt)), crate::estimate_tokens(&c.content).max(1));
        }
        Ok(c)
    }
}
