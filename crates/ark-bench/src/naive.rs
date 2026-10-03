//! The naive baseline: the agent loop an application usually starts with, to measure ARK
//! against. Every request sends one fixed system prompt, the whole transcript and every
//! tool's full schema. A tool result goes back whole. Nothing is planned, deduplicated,
//! distilled, compacted or stopped early, except by a step cap. Transient provider errors are
//! retried as a provider SDK does (`RetryPolicy::default`).
//!
//! It runs the same tools on the same store with the same model and is judged by the same
//! checks, so a difference in a report is the harness. It emits the [`HarnessEvent`]s the
//! benchmark measures (`ModelCall`, `ToolCall`, `ToolResult`) and nothing else.

use std::time::Instant;

use ark_sdk::prelude::ark_model::{
    estimate_tokens, Message, RetryPolicy, Role as MsgRole, ToolDef,
};
use ark_sdk::prelude::ark_runtime::catalog::action_tooldef;
use ark_sdk::prelude::*;

/// The system prompt. Short and generic, as a first version of an app's prompt is.
pub const SYSTEM: &str = "You are a helpful assistant. Use the tools when they help. \
When you have the answer, reply with it in plain text.";

/// Model calls a turn may make before it stops without an answer.
pub const MAX_STEPS: usize = 25;

/// One conversation. The transcript outlives a restart, as an app that saves it to a file
/// would keep it.
#[derive(Default)]
pub struct Naive {
    pub transcript: Vec<Message>,
}

/// What one naive turn produced.
pub struct NaiveTurn {
    pub answer: String,
    pub steps: usize,
}

impl Naive {
    /// Run one turn: append `input`, call the model until it answers in text or the step cap
    /// is reached, running every tool call it asks for.
    pub async fn turn(
        &mut self,
        ark: &Ark,
        project: &ProjectId,
        input: &str,
        on_event: &mut (dyn FnMut(HarnessEvent) + Send),
    ) -> anyhow::Result<NaiveTurn> {
        let provider = ark.router().provider(Role::Reason).clone();
        // The app's tools, and the web when the app turned it on, as an app would offer them.
        let scoped;
        let engine = match ark.web_tools(&provider) {
            Some(web) => {
                let base = ark.engine();
                scoped = base.scoped(base.registry.clone().with(web.adapter));
                &scoped
            }
            None => ark.engine(),
        };
        let mut tools: Vec<ToolDef> = engine
            .registry
            .actions()
            .iter()
            .map(action_tooldef)
            .collect();
        tools.sort_by(|a, b| a.name.cmp(&b.name));
        self.transcript.push(Message::user(input));
        let retry = RetryPolicy::default();
        for step in 1..=MAX_STEPS {
            let started = Instant::now();
            let mut attempt = 0;
            let completion = loop {
                match provider.complete(SYSTEM, &self.transcript, &tools).await {
                    Ok(c) => break c,
                    Err(e) if retry.should_retry(&e, attempt) => {
                        tokio::time::sleep(retry.delay(attempt, &e)).await;
                        attempt += 1;
                    }
                    Err(e) => return Err(e.into()),
                }
            };
            on_event(HarnessEvent::ModelCall {
                role: "reason".into(),
                model: provider.model_id(),
                usage: completion.usage,
                elapsed_ms: started.elapsed().as_millis() as u64,
                first_token_ms: None,
                retries: attempt,
            });
            if completion.tool_calls.is_empty() {
                self.transcript
                    .push(Message::assistant(completion.content.clone()));
                return Ok(NaiveTurn {
                    answer: completion.content,
                    steps: step,
                });
            }
            self.transcript.push(Message {
                role: MsgRole::Assistant,
                content: completion.content.clone(),
                tool_calls: completion.tool_calls.clone(),
                tool_call_id: None,
                tool_name: None,
            });
            for call in &completion.tool_calls {
                on_event(HarnessEvent::ToolCall {
                    id: call.id.clone(),
                    action: call.name.clone(),
                    input: call.arguments.clone(),
                });
                let ran = Instant::now();
                let (ok, text, invocation, error) = match engine
                    .run_action(project, &call.name, call.arguments.clone(), true)
                    .await
                {
                    Ok(inv) => match &inv.error {
                        Some(e) => (
                            false,
                            format!("failed: {e}"),
                            inv.id.to_string(),
                            Some(e.clone()),
                        ),
                        None => (true, inv.output.to_string(), inv.id.to_string(), None),
                    },
                    Err(e) => (
                        false,
                        format!("failed: {e}"),
                        String::new(),
                        Some(e.to_string()),
                    ),
                };
                let tokens = estimate_tokens(&text);
                on_event(HarnessEvent::ToolResult {
                    id: call.id.clone(),
                    action: call.name.clone(),
                    ok,
                    summary: text.chars().take(120).collect(),
                    invocation,
                    duration_ms: Some(ran.elapsed().as_millis() as i64),
                    error,
                    context_tokens: tokens,
                    raw_tokens: tokens,
                });
                self.transcript.push(Message::tool_result(call, text));
            }
        }
        Ok(NaiveTurn {
            answer: String::new(),
            steps: MAX_STEPS,
        })
    }
}
