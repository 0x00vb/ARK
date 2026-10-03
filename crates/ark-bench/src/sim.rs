//! A simulated model for deterministic runs. A [`Policy`] looks at what the request actually
//! contains (the tools offered, the tool results still in the window, the working state) and
//! decides the next move, so a runtime change that hides a result, offers a tool earlier or
//! wastes a step changes what the simulated agent does and what the run costs.
//!
//! Usage is simulated too: the prompt is the estimated size of system, messages and tool
//! schemas, and the cached part is the prefix shared with a recent request when it is at
//! least [`CACHE_MIN_TOKENS`] long (prefix caching as Anthropic and OpenAI implement it).
//! Simulated numbers are marked `measured: false`.

use std::sync::{Arc, Mutex};

use ark_sdk::prelude::ark_model::{
    estimate_tokens, Completion, Message, Provider, Result as ModelResult, Role, TextSink,
    ToolCall, ToolDef, Usage,
};
use async_trait::async_trait;
use serde_json::{json, Value};

use crate::record::{message_tokens, render, tool_schema_tokens, PrefixTracker};

/// The shortest prefix a provider caches.
pub const CACHE_MIN_TOKENS: u32 = 1024;

/// Decides the simulated model's reply to one request.
pub type Policy = Arc<dyn Fn(&SimRequest) -> Reply + Send + Sync>;

/// What the simulated model does.
pub enum Reply {
    Text(String),
    Calls(Vec<(String, Value)>),
}

impl Reply {
    pub fn call(action: &str, args: Value) -> Reply {
        Reply::Calls(vec![(action.to_string(), args)])
    }
}

/// A read-only view of one request, with the questions a policy asks of it.
pub struct SimRequest<'a> {
    pub system: &'a str,
    pub messages: &'a [Message],
    pub tools: &'a [ToolDef],
}

impl<'a> SimRequest<'a> {
    pub fn has_tool(&self, name: &str) -> bool {
        self.tools.iter().any(|t| t.name == name)
    }
    /// The schema of an offered tool.
    pub fn tool_schema(&self, name: &str) -> Option<&Value> {
        self.tools
            .iter()
            .find(|t| t.name == name)
            .map(|t| &t.parameters)
    }
    /// Whether `needle` appears in the system prompt or any message.
    pub fn mentions(&self, needle: &str) -> bool {
        self.system.contains(needle) || self.messages.iter().any(|m| m.content.contains(needle))
    }
    /// The planner's request: no tools, and the system prompt asks for a plan.
    pub fn is_planner(&self) -> bool {
        self.tools.is_empty()
            && self.system.contains("plan")
            && self.messages.len() == 1
            && self.messages[0].content.starts_with("Goal:")
    }
    /// The critic's request.
    pub fn is_critic(&self) -> bool {
        self.tools.is_empty()
            && self
                .messages
                .last()
                .is_some_and(|m| m.content.contains("== DRAFT ANSWER =="))
    }
    /// The draft the critic was asked to review.
    pub fn draft(&self) -> String {
        self.messages
            .last()
            .and_then(|m| m.content.split("== DRAFT ANSWER ==").nth(1))
            .unwrap_or("")
            .trim()
            .to_string()
    }
    /// The task of this turn: the text after `== TASK ==` (ARK marks it when it adds
    /// context), else the first user message after the last finished turn (an assistant reply
    /// in text with no tool calls). In a plain transcript that is this turn's input; within one
    /// harness turn, later user messages are the harness's own nudges, not the task.
    pub fn task(&self) -> String {
        if let Some(m) = self
            .messages
            .iter()
            .find(|m| m.role == Role::User && m.content.contains("== TASK =="))
        {
            return m
                .content
                .split("== TASK ==")
                .last()
                .unwrap_or("")
                .trim()
                .to_string();
        }
        let start = self
            .messages
            .iter()
            .rposition(|m| {
                m.role == Role::Assistant && m.tool_calls.is_empty() && !m.content.is_empty()
            })
            .map(|i| i + 1)
            .unwrap_or(0);
        self.messages[start..]
            .iter()
            .find(|m| m.role == Role::User)
            .map(|m| m.content.trim().to_string())
            .unwrap_or_default()
    }
    /// Tool results still in the window: (action, text), oldest first.
    pub fn results(&self) -> Vec<(&str, &str)> {
        self.messages
            .iter()
            .filter(|m| m.role == Role::Tool)
            .map(|m| (m.tool_name.as_deref().unwrap_or(""), m.content.as_str()))
            .collect()
    }
    /// The latest visible result of `action`.
    pub fn result_of(&self, action: &str) -> Option<&str> {
        self.results()
            .into_iter()
            .rev()
            .find(|(a, _)| *a == action)
            .map(|(_, t)| t)
    }
    /// Calls the model already made in the visible window: (action, args).
    pub fn calls(&self) -> Vec<(&str, &Value)> {
        self.messages
            .iter()
            .flat_map(|m| m.tool_calls.iter())
            .map(|c| (c.name.as_str(), &c.arguments))
            .collect()
    }
    pub fn count_calls(&self, action: &str) -> usize {
        self.calls().iter().filter(|(a, _)| *a == action).count()
    }
    /// Whether this exact call is visible with a result after it.
    pub fn called_with(&self, action: &str, args: &Value) -> bool {
        self.calls().iter().any(|(a, v)| *a == action && *v == args)
    }
    /// The `[inv:...]` reference at the start of a result, if it has one.
    pub fn inv_of(result: &str) -> Option<String> {
        let start = result.find("[inv:")? + 5;
        let end = result[start..].find(']')? + start;
        Some(result[start..end].to_string())
    }
    /// Every `[inv:...]` reference visible in tool results.
    pub fn cited(&self) -> Vec<String> {
        self.results()
            .iter()
            .filter_map(|(_, t)| Self::inv_of(t))
            .collect()
    }
    /// All visible text: system plus every message.
    pub fn visible(&self) -> String {
        let mut s = self.system.to_string();
        for m in self.messages {
            s.push('\n');
            s.push_str(&m.content);
        }
        s
    }
}

/// The token after `prefix` in `text` (letters, digits, `.`, `_`, `-`). A `key=` prefix also
/// matches the raw JSON form (`"key": value` or `"key":"value"`), so a policy reads a result
/// the same whether a harness distilled it into a card or passed the raw output.
pub fn word_after(text: &str, prefix: &str) -> Option<String> {
    let token = |rest: &str| {
        let w: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
            .collect();
        (!w.is_empty()).then_some(w)
    };
    if let Some(i) = text.find(prefix) {
        return token(&text[i + prefix.len()..]);
    }
    let key = prefix.strip_suffix('=')?;
    let i = text.find(&format!("\"{key}\":"))? + key.len() + 3;
    token(text[i..].trim_start().trim_start_matches('"'))
}

/// A scripted model that answers through a [`Policy`] and simulates usage.
pub struct SimProvider {
    id: String,
    policy: Policy,
    recorder: Option<Arc<crate::record::Recorder>>,
    prefix: Mutex<PrefixTracker>,
    calls: Mutex<u64>,
}

impl SimProvider {
    /// A shared provider driven by `policy`. Simulated cache writes go to `recorder`.
    pub fn shared(
        id: impl Into<String>,
        policy: Policy,
        recorder: Option<Arc<crate::record::Recorder>>,
    ) -> Arc<dyn Provider> {
        Arc::new(SimProvider {
            id: id.into(),
            policy,
            recorder,
            prefix: Mutex::new(PrefixTracker::default()),
            calls: Mutex::new(0),
        })
    }

    fn reply(&self, system: &str, messages: &[Message], tools: &[ToolDef]) -> Completion {
        let req = SimRequest {
            system,
            messages,
            tools,
        };
        let n = {
            let mut c = self.calls.lock().unwrap();
            *c += 1;
            *c
        };
        let (content, tool_calls) = match (self.policy)(&req) {
            Reply::Text(t) => (t, Vec::new()),
            Reply::Calls(calls) => (
                String::new(),
                calls
                    .into_iter()
                    .enumerate()
                    .map(|(i, (name, arguments))| ToolCall {
                        id: format!("sim_{n}_{i}"),
                        name,
                        arguments,
                    })
                    .collect(),
            ),
        };
        let prompt = estimate_tokens(system) + message_tokens(messages) + tool_schema_tokens(tools);
        let shared = self
            .prefix
            .lock()
            .unwrap()
            .observe(render(system, messages, tools));
        let cached = if shared >= CACHE_MIN_TOKENS {
            shared.min(prompt)
        } else {
            0
        };
        // What a breakpoint before the last message writes: everything up to it that was not
        // read from the cache, once the prefix is long enough to cache at all.
        let last = messages
            .last()
            .map(|m| message_tokens(std::slice::from_ref(m)))
            .unwrap_or(0);
        let written = if prompt.saturating_sub(last) >= CACHE_MIN_TOKENS {
            prompt.saturating_sub(last).saturating_sub(cached)
        } else {
            0
        };
        if let Some(r) = &self.recorder {
            r.add_sim_write(written);
        }
        let completion = estimate_tokens(&content)
            + tool_calls
                .iter()
                .map(|c: &ToolCall| {
                    estimate_tokens(&c.name) + estimate_tokens(&c.arguments.to_string()) + 8
                })
                .sum::<u32>();
        Completion {
            content,
            tool_calls,
            usage: Usage {
                prompt_tokens: prompt,
                completion_tokens: completion.max(1),
                measured: false,
                cached_tokens: cached,
                // Builds against runtimes with and without newer usage fields.
                ..Default::default()
            },
        }
    }
}

#[async_trait]
impl Provider for SimProvider {
    async fn complete(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> ModelResult<Completion> {
        Ok(self.reply(system, messages, tools))
    }

    async fn complete_streaming(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDef],
        on_text: TextSink<'_>,
    ) -> ModelResult<Completion> {
        let c = self.reply(system, messages, tools);
        for chunk in c.content.split_inclusive(' ') {
            on_text(chunk);
        }
        Ok(c)
    }

    fn describe(&self) -> String {
        format!("simulated {}", self.id)
    }
    fn model_id(&self) -> String {
        self.id.clone()
    }
}

/// One thing the simulated agent needs before it can answer.
#[derive(Clone, Debug)]
pub enum Goal {
    /// Run `action` with `args`, discovering it first if its schema is not offered.
    Call { action: String, args: Value },
    /// See `needle` in a tool result. If the result of `from` does not show it, fetch more of
    /// that result with `get_evidence`, at most twice. `filter` is what a model would search
    /// for when the tool offers a filter (it does not know the needle in advance).
    Find {
        needle: String,
        from: String,
        filter: String,
    },
}

impl Goal {
    pub fn call(action: &str, args: Value) -> Goal {
        Goal::Call {
            action: action.into(),
            args,
        }
    }
    pub fn find(needle: &str, from: &str, filter: &str) -> Goal {
        Goal::Find {
            needle: needle.into(),
            from: from.into(),
            filter: filter.into(),
        }
    }
}

/// How the competent simulated agent answers once its goals are met (or abandoned).
pub type Answer = Arc<dyn Fn(&SimRequest) -> String + Send + Sync>;

/// A simulated agent that works like a careful model: it reads the capability index, loads an
/// action before calling it, runs each needed call once, digs into a result when the summary
/// is not enough, cites what it used, and answers. `goals` reads the request, so later goals
/// can depend on what earlier results showed.
pub fn competent(
    goals: impl Fn(&SimRequest) -> Vec<Goal> + Send + Sync + 'static,
    answer: Answer,
) -> Policy {
    Arc::new(move |req: &SimRequest| {
        if req.is_planner() {
            return Reply::Text(
                "- find the relevant tool\n- run it\n- answer from its result".into(),
            );
        }
        if req.is_critic() {
            return Reply::Text(req.draft());
        }
        if req.tools.is_empty() {
            return Reply::Text(answer(req));
        }
        for goal in goals(req) {
            match goal {
                Goal::Call { action, args } => {
                    if req.called_with(&action, &args) && req.result_of(&action).is_some() {
                        continue;
                    }
                    if req.has_tool(&action) {
                        return Reply::call(&action, args);
                    }
                    if req.mentions(&action) && req.has_tool("load_action") {
                        return Reply::call("load_action", json!({ "name": action }));
                    }
                    // Cannot find it: give up on this goal.
                    break;
                }
                Goal::Find {
                    needle,
                    from,
                    filter,
                } => {
                    if req.results().iter().any(|(_, t)| t.contains(&needle)) {
                        continue;
                    }
                    let attempts = req.count_calls("get_evidence");
                    let Some(inv) = req.result_of(&from).and_then(SimRequest::inv_of) else {
                        break;
                    };
                    if attempts >= 2 {
                        break;
                    }
                    let schema = req.tool_schema("get_evidence").cloned().unwrap_or_default();
                    let props = &schema["properties"];
                    let mut args = json!({ "invocation": format!("inv:{inv}") });
                    // A careful model uses a filter when the schema offers one.
                    if props.get("match").is_some() {
                        args["match"] = json!(filter);
                    } else if attempts == 1 {
                        args["limit"] = json!(5000);
                    }
                    return Reply::call("get_evidence", args);
                }
            }
        }
        // Before answering, record the finding the way the default instructions ask: once per
        // task, citing what it rests on. This is what makes the working state change mid-turn.
        let cited = req.cited();
        if !cited.is_empty() && req.has_tool("record_state") && req.count_calls("record_state") == 0
        {
            let text = answer(req);
            return Reply::call(
                "record_state",
                json!({
                    "kind": "finding",
                    "text": text.split(" [inv:").next().unwrap_or(&text),
                    "claim_type": "fact",
                    "cites": cited.iter().map(|i| format!("inv:{i}")).collect::<Vec<_>>(),
                }),
            );
        }
        Reply::Text(answer(req))
    })
}

/// Cite every visible result, in the agent's usual format.
pub fn cite_all(req: &SimRequest) -> String {
    let refs: Vec<String> = req.cited().iter().map(|i| format!("[inv:{i}]")).collect();
    refs.join(" ")
}
