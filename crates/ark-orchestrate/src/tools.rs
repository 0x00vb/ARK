//! The tools an agent uses to work with other agents. Built for one agent and one task, so
//! every call knows who is asking and from which task:
//!
//! - `agents.delegate(to, objective, context, artifacts, wait)`: create a task for a
//!   permitted agent. With `wait` (the default) the call returns the result; several
//!   delegations in one reply run in parallel.
//! - `agents.wait(tasks)`: wait for tasks delegated with `wait: false`.
//! - `agents.message(to, text)`: a short note to a permitted agent, delivered into its running
//!   task or kept for its next one.
//!
//! The person's own conversation gets [`ChatTools`] instead: create an agent, hand any agent a
//! task, wait for tasks, have an agent run a task at set times. So the person asks in words and
//! the chat decides what becomes a task. With [`BoardTools`] it also reads and plans the
//! project's board (`board.list`, `board.add`, `board.update`); a task handed off with `item`
//! moves that item (see [`crate::board`]).
//!
//! The calls are ordinary actions: each becomes an invocation in the delegating agent's
//! store and an evidence card in its context, so a delegation is traced like any tool call
//! and its result is cited like any evidence.

use std::sync::{Arc, Weak};

use ark_core::{ActionDescriptor, RiskTier};
use ark_runtime::Distiller;
use ark_tools::{ActionOutput, Adapter, AdapterError, RunCtx};
use serde_json::{json, Value};

use crate::board::{self, WorkChange, WorkItem, WorkState};
use crate::group::{self, Candidate, Request};
use crate::orchestrator::{valid_name, Inner};
use crate::record::{AgentState, TaskRecord};
use crate::spec::{AgentSpec, TaskSpec};
use crate::Orchestrator;

pub const DELEGATE: &str = "agents.delegate";
pub const WAIT: &str = "agents.wait";
pub const MESSAGE: &str = "agents.message";
pub const CREATE: &str = "agents.create";
pub const ROLE_SET: &str = "role.set";
pub const GROUPS_SET: &str = "groups.set";
pub const WORK_LATER: &str = "work.later";
pub const START: &str = "agents.start";
pub const SCHEDULE: &str = "agents.schedule";
pub const UNSCHEDULE: &str = "agents.unschedule";
pub const BOARD_LIST: &str = "board.list";
pub const BOARD_ADD: &str = "board.add";
pub const BOARD_UPDATE: &str = "board.update";

/// Who the person's conversation is on the board.
pub const CHAT: &str = "chat";

/// Open items `board.list` returns at most; each board's Done shows the latest few.
pub const BOARD_LISTED: usize = 40;
const DONE_LISTED: usize = 5;

/// Characters of a delegated task's result in the delegator's evidence card. The full result
/// stays in the invocation (`get_evidence`).
pub const RESULT_CHARS: usize = 2400;

/// `role.set`: the agent's standing instructions, as its person gives them in chat. Registered
/// only for a task the person typed (ADR 0116). It never touches `AgentSpec::role`.
pub(crate) struct RoleTools {
    pub orch: Weak<Inner>,
    pub agent: String,
    pub task: String,
}

#[async_trait::async_trait]
impl Adapter for RoleTools {
    fn name(&self) -> &str {
        "role"
    }

    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![ActionDescriptor::new(
            ROLE_SET,
            "Set your standing instructions, kept for every later message: who you are or how you must always work, as the person just stated it. Pass the full text (merge any earlier text you keep); empty text clears it. Use it only when the person's own message says who you are or how you always work, never for a one-off request, and call it before any other tool. Then answer in one short line that you will.",
            // It changes only this agent's own instructions, from the person's own words (the
            // first call of the turn), so it never asks: the owner chose no card (ADR 0116).
            RiskTier::Observe,
        )
        .volatile()
        .ends_with_short_reply()
        .with_input_schema(json!({
            "type": "object",
            "required": ["instructions"],
            "properties": {
                "instructions": { "type": "string", "maxLength": crate::spec::STANDING_MAX_CHARS }
            }
        }))]
    }

    async fn run(
        &self,
        action: &str,
        input: Value,
        _ctx: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        let text = input
            .get("instructions")
            .and_then(Value::as_str)
            .ok_or_else(|| AdapterError::bad_input(action, "instructions is text"))?;
        if text.trim().chars().count() > crate::spec::STANDING_MAX_CHARS {
            return Err(AdapterError::bad_input(
                action,
                format!(
                    "instructions: at most {} characters",
                    crate::spec::STANDING_MAX_CHARS
                ),
            ));
        }
        let orch = self
            .orch
            .upgrade()
            .ok_or_else(|| AdapterError::Tool("the orchestrator is gone".into()))?;
        if orch.read_before(&self.task, ROLE_SET) {
            return Err(AdapterError::Tool(
                "Set your role only from the person's own message, before reading anything else."
                    .into(),
            ));
        }
        let stored = orch
            .set_standing(&self.agent, text)
            .map_err(|e| AdapterError::Tool(e.to_string()))?;
        let said = if stored.is_empty() {
            "Standing instructions cleared from the person's next message.".to_string()
        } else {
            format!(
                "Standing instructions kept from the person's next message (this turn keeps its prompt): {}",
                crate::brief::clip(&stored, 300)
            )
        };
        Ok(ActionOutput::json(json!({ "result": said })))
    }
}

/// `groups.set`: a group of agents with shared rules, as its person asks for it in chat.
/// Registered only for a task the person typed (ADR 0117), like `role.set`.
pub(crate) struct GroupTools {
    pub orch: Weak<Inner>,
    pub agent: String,
    pub task: String,
}

impl GroupTools {
    fn request(input: &Value) -> Result<Request, String> {
        let text = |v: &Value, k: &str| {
            v.get(k)
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string()
        };
        let name = text(input, "name");
        if name.is_empty() || name.chars().count() > group::NAME_CHARS {
            return Err(format!("name: 1 to {} characters", group::NAME_CHARS));
        }
        let rules: Option<Vec<String>> = match input.get("rules") {
            None => None,
            Some(v) => Some(
                v.as_array()
                    .ok_or("rules is a list of texts")?
                    .iter()
                    .map(|r| r.as_str().map(|r| r.trim().to_string()))
                    .collect::<Option<_>>()
                    .ok_or("rules is a list of texts")?,
            ),
        };
        if rules.as_ref().is_some_and(|rules| {
            rules.len() > group::MAX_RULES
                || rules
                    .iter()
                    .any(|r| r.is_empty() || r.chars().count() > group::RULE_CHARS)
        }) {
            return Err(format!(
                "rules: at most {} rules of 1 to {} characters",
                group::MAX_RULES,
                group::RULE_CHARS
            ));
        }
        let listed = input
            .get("members")
            .and_then(Value::as_array)
            .ok_or("members is a list")?;
        if !(1..=8).contains(&listed.len()) {
            return Err("members: 1 to 8 agents".into());
        }
        let mut members: Vec<Candidate> = Vec::new();
        for m in listed {
            let name = text(m, "name").to_lowercase().replace(' ', "-");
            if name.contains('/') {
                return Err(format!("`{name}`: only agents of this folder can join"));
            }
            if !valid_name(&name) {
                return Err(format!("`{name}` is not a valid agent name"));
            }
            if members.iter().any(|c| c.name == name) {
                return Err(format!("`{name}` is listed twice"));
            }
            let role = text(m, "role");
            let personality = text(m, "personality");
            if role.chars().count() > 500
                || personality.chars().count() > crate::spec::STANDING_MAX_CHARS
            {
                return Err(format!(
                    "role: at most 500 characters; personality: at most {}",
                    crate::spec::STANDING_MAX_CHARS
                ));
            }
            members.push(Candidate {
                name,
                role,
                personality,
                selection: crate::execution::ExecutionSelection::parse(m)
                    .map_err(|e| e.to_string())?,
            });
        }
        Ok(Request {
            name,
            rules,
            members,
        })
    }
}

#[async_trait::async_trait]
impl Adapter for GroupTools {
    fn name(&self) -> &str {
        "groups"
    }

    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![ActionDescriptor::new(
            GROUPS_SET,
            "Make or update a group of agents with rules they all follow, as the person just asked: a team they want (an editor and a researcher, say). The same group name updates that group: pass the full list of rules to change them (omit to keep them); listed members are added, others stay. New names become agents with their own role and personality; names of existing agents join and keep theirs. Members may message each other. Use it only when the person's own message asks for a group or team, never for a one-off request, and call it before any other tool. Then say in one line who is in it.",
            RiskTier::Observe,
        )
        .volatile()
        .ends_with_short_reply()
        .with_input_schema(json!({
            "type": "object",
            "required": ["name", "members"],
            "properties": {
                "name": { "type": "string", "maxLength": group::NAME_CHARS },
                "rules": {
                    "type": "array",
                    "maxItems": group::MAX_RULES,
                    "items": { "type": "string", "maxLength": group::RULE_CHARS },
                    "description": "the full list of what every member always follows, from the person's words; replaces the current list"
                },
                "members": {
                    "type": "array", "minItems": 1, "maxItems": 8,
                    "items": {
                        "type": "object", "required": ["name"],
                        "properties": {
                            "name": { "type": "string", "description": "short lowercase name" },
                            "role": { "type": "string", "maxLength": 500, "description": "one line; required for a new agent" },
                            "personality": { "type": "string", "maxLength": crate::spec::STANDING_MAX_CHARS, "description": "who it is and how it works, in the person's words; new agents only" },
                            "model": { "type": "string", "description": "provider:model; omitted inherits this turn; new agents only" }
                        }
                    }
                }
            }
        }))]
    }

    async fn run(
        &self,
        action: &str,
        input: Value,
        _ctx: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        let request = Self::request(&input).map_err(|e| AdapterError::bad_input(action, e))?;
        let orch = self
            .orch
            .upgrade()
            .ok_or_else(|| AdapterError::Tool("the orchestrator is gone".into()))?;
        if orch.read_before(&self.task, GROUPS_SET) {
            return Err(AdapterError::Tool(
                "Make a group only from the person's own message, before reading anything else."
                    .into(),
            ));
        }
        let outcome = orch
            .set_group(&self.agent, &self.task, request)
            .await
            .map_err(|e| AdapterError::Tool(e.to_string()))?;
        Ok(ActionOutput::json(
            serde_json::to_value(outcome).map_err(|e| AdapterError::Tool(e.to_string()))?,
        ))
    }
}

/// `work.later`: hand the turn's own work to the background and answer later in the same chat
/// (ADR 0118). Registered only for a chat turn: the person's or a group chat's.
pub(crate) struct LaterTools {
    pub orch: Weak<Inner>,
    pub agent: String,
    pub task: String,
}

#[async_trait::async_trait]
impl Adapter for LaterTools {
    fn name(&self) -> &str {
        "work"
    }

    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![ActionDescriptor::new(
            WORK_LATER,
            "Hand your own work to the background when answering needs more than a few quick steps (research, a long read, a build). Write a self-contained brief: the work does not see this chat. It runs as you, and its answer arrives in this chat by itself as your next message. Call it once, then reply to the person in one short line that you will look into it, and stop. Never use it for a question you can answer now.",
            RiskTier::Observe,
        )
        .volatile()
        .ends_with_short_reply()
        .with_input_schema(json!({
            "type": "object",
            "required": ["brief"],
            "properties": {
                "brief": { "type": "string", "maxLength": 8000, "description": "what to find out or do, with everything it needs" }
            }
        }))]
    }

    async fn run(
        &self,
        action: &str,
        input: Value,
        _ctx: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        let brief = input
            .get("brief")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if brief.is_empty() || brief.chars().count() > 8000 {
            return Err(AdapterError::bad_input(
                action,
                "brief: 1 to 8000 characters",
            ));
        }
        let orch = self
            .orch
            .upgrade()
            .ok_or_else(|| AdapterError::Tool("the orchestrator is gone".into()))?;
        let already = orch
            .work_later(&self.agent, &self.task, brief)
            .await
            .map_err(|e| AdapterError::Tool(e.to_string()))?;
        Ok(ActionOutput::json(json!({
            "started": true,
            "already": already,
            "note": "Its answer will arrive in this chat as your next message. Tell the person in one short line that you will look into it, and stop."
        })))
    }
}

pub(crate) struct AgentTools {
    pub orch: Weak<Inner>,
    pub agent: String,
    pub task: String,
    /// Agents it may delegate to: (name, role).
    pub delegates: Vec<(String, String)>,
    /// Agents it may message or start: (name, title).
    pub contacts: Vec<(String, String)>,
    pub can_create: bool,
    /// On a chat turn: the schedule tools it may have, and whether the chat is in Auto
    /// (ADR 0120).
    pub schedule: Option<ScheduleOffer>,
    /// Colleagues it starts continue its chat when they finish (ADR 0126).
    pub comes_back: bool,
}

/// The schedule tools a chat turn of a named agent has.
#[derive(Clone, Copy)]
pub(crate) struct ScheduleOffer {
    pub schedule: bool,
    pub unschedule: bool,
    pub auto: bool,
}

impl AgentTools {
    fn contact_list(&self) -> String {
        let pairs: Vec<(&str, &str)> = self
            .contacts
            .iter()
            .map(|(n, t)| (n.as_str(), t.as_str()))
            .collect();
        crate::names::listed(&pairs, |_| true)
    }

    /// Run `f` with the schedule tools of this turn's chat, if it has them.
    fn scheduling<T>(&self, f: impl FnOnce(&Scheduling<'_>, ScheduleOffer) -> T) -> Option<T> {
        let offer = self.schedule?;
        let orch = Orchestrator {
            inner: self.orch.upgrade()?,
        };
        let chat = orch.inner.chat_of_turn(&self.task);
        let view = Scheduling {
            orch: &orch,
            chat: chat.as_deref(),
            zone: None,
            by: Some(&self.agent),
            auto: offer.auto,
        };
        Some(f(&view, offer))
    }
}

#[async_trait::async_trait]
impl Adapter for AgentTools {
    fn name(&self) -> &str {
        "agents"
    }

    fn actions(&self) -> Vec<ActionDescriptor> {
        let mut v = Vec::new();
        if self.can_create {
            v.push(ActionDescriptor::new(
                CREATE,
                "create a persistent colleague with inherited permissions; use agents.start for its first brief",
                RiskTier::Observe,
            ).volatile().with_input_schema(json!({
                "type": "object", "required": ["name", "role"],
                "properties": {
                    "name": { "type": "string", "description": "short lowercase name" },
                    "role": { "type": "string" },
                    "model": { "type": "string", "description": "provider:model (account allowed); omitted inherits this turn" },
                    "reasoning": { "type": "string", "description": "supported effort; omitted inherits unless model changes" },
                    "instructions": { "type": "string", "description": "role instructions only; no conversation transcript" }
                }
            })));
        }
        if self.can_create || !self.contacts.is_empty() {
            let start = ActionDescriptor::new(
            START,
            "start an independent conversation with a colleague",
            RiskTier::Observe,
        ).volatile().with_input_schema(json!({
            "type": "object", "required": ["to", "objective"],
            "properties": {
                "to": { "type": "string", "description": format!("the agent's name: {}; also agents you created", self.contact_list()) },
                "objective": { "type": "string", "maxLength": 8000, "description": if self.comes_back { "self-contained brief; returns immediately; when every colleague you started has finished, their results come back to this chat and you continue" } else { "self-contained brief; returns immediately, no waiting or automatic reply" } },
                "context": { "type": "string", "maxLength": 16000, "description": "necessary information only; the recipient does not see your transcript" }
            }
        }));
            // Results come back to the chat: a one-line "when they report back" closes the turn
            // (ADR 0126).
            v.push(if self.comes_back {
                start.ends_with_short_reply()
            } else {
                start
            });
        }
        if !self.delegates.is_empty() {
            let names: Vec<&str> = self.delegates.iter().map(|(n, _)| n.as_str()).collect();
            v.push(
                ActionDescriptor::new(
                    DELEGATE,
                    "hand a self-contained task to another agent and get its result; independent tasks can be delegated in one reply and run in parallel",
                    RiskTier::Observe,
                )
                .volatile()
                .with_input_schema(json!({
                    "type": "object",
                    "required": ["to", "objective"],
                    "properties": {
                        "to": { "type": "string", "enum": names },
                        "objective": { "type": "string", "description": "what the agent must achieve" },
                        "context": { "type": "string", "description": "only what it needs to know; it does not see your conversation" },
                        "artifacts": { "type": "array", "items": { "type": "string" }, "description": "art:<id> of artifacts it may use" },
                        "wait": { "type": "boolean", "description": "wait for the result (default true); false returns the task id at once" }
                    }
                })),
            );
            v.push(
                ActionDescriptor::new(
                    WAIT,
                    "wait for delegated tasks and get their results",
                    RiskTier::Observe,
                )
                .volatile()
                .with_input_schema(json!({
                    "type": "object",
                    "required": ["tasks"],
                    "properties": { "tasks": { "type": "array", "items": { "type": "string" } } }
                })),
            );
        }
        if self.can_create || !self.contacts.is_empty() {
            v.push(
                ActionDescriptor::new(
                    MESSAGE,
                    "send a short note to another agent; to give it work, use agents.start",
                    RiskTier::Observe,
                )
                .volatile()
                .with_input_schema(json!({
                    "type": "object",
                    "required": ["to", "text"],
                    "properties": {
                        "to": { "type": "string", "description": format!("the agent's name: {}{}", self.contact_list(), if self.can_create { "; also agents you created" } else { "" }) },
                        "text": { "type": "string" }
                    }
                })),
            );
        }
        v.extend(
            self.scheduling(|view, offer| view.actions(offer.schedule, offer.unschedule))
                .unwrap_or_default(),
        );
        v
    }

    async fn run(
        &self,
        action: &str,
        input: Value,
        _ctx: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        let orch = self
            .orch
            .upgrade()
            .ok_or_else(|| AdapterError::Tool("the orchestrator is gone".into()))?;
        let s = |k: &str| input.get(k).and_then(Value::as_str).unwrap_or("").trim();
        match action {
            SCHEDULE | UNSCHEDULE => {
                let offer = self.schedule.filter(|o| {
                    if action == SCHEDULE {
                        o.schedule
                    } else {
                        o.unschedule
                    }
                });
                if offer.is_none() {
                    return Err(AdapterError::UnknownAction(action.to_string()));
                }
                let orch = Orchestrator { inner: orch };
                let chat = orch.inner.chat_of_turn(&self.task);
                Scheduling {
                    orch: &orch,
                    chat: chat.as_deref(),
                    zone: None,
                    by: Some(&self.agent),
                    auto: offer.is_some_and(|o| o.auto),
                }
                .run(action, input)
                .await
            }
            CREATE => {
                let name = s("name").to_lowercase().replace(' ', "-");
                if name.len() > 64
                    || s("role").is_empty()
                    || s("role").chars().count() > 500
                    || s("instructions").chars().count() > 16000
                {
                    return Err(AdapterError::bad_input(action, "name: at most 64 bytes; role: 1 to 500 characters; instructions: at most 16000 characters"));
                }
                let a = orch
                    .create_colleague(
                        &self.agent,
                        &self.task,
                        &name,
                        s("role"),
                        s("instructions"),
                        crate::execution::ExecutionSelection::parse(&input)
                            .map_err(|e| AdapterError::bad_input(action, e.to_string()))?,
                    )
                    .await
                    .map_err(|e| AdapterError::Tool(e.to_string()))?;
                Ok(ActionOutput::json(
                    json!({ "agent": a.name, "role": a.spec.role, "state": a.state }),
                ))
            }
            START => {
                let to = orch
                    .agent_named(&self.agent, &self.task, s("to"))
                    .map_err(|e| AdapterError::bad_input(action, e))?;
                if s("objective").is_empty()
                    || s("objective").chars().count() > 8000
                    || s("context").chars().count() > 16000
                {
                    return Err(AdapterError::bad_input(
                        action,
                        "objective: 1 to 8000 characters; context: at most 16000 characters",
                    ));
                }
                let id = orch
                    .start_conversation(
                        &self.agent,
                        &self.task,
                        &to,
                        TaskSpec::new(s("objective")).context(s("context")),
                    )
                    .await
                    .map_err(|e| AdapterError::Tool(e.to_string()))?;
                Ok(ActionOutput::json(
                    json!({ "task": id, "agent": to, "independent": true, "comes_back": self.comes_back }),
                ))
            }
            DELEGATE => {
                let delegates: Vec<String> =
                    self.delegates.iter().map(|(n, _)| n.clone()).collect();
                let to = orch
                    .delegate_named(&self.agent, &delegates, s("to"))
                    .map_err(|e| AdapterError::bad_input(action, e))?;
                let to = to.as_str();
                let artifacts: Vec<String> = input
                    .get("artifacts")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(|x| x.trim_start_matches("art:").to_string())
                            .collect()
                    })
                    .unwrap_or_default();
                let spec = TaskSpec::new(s("objective"))
                    .context(s("context"))
                    .artifacts(artifacts);
                if spec.objective.is_empty() {
                    return Err(AdapterError::bad_input(action, "an objective is required"));
                }
                let wait = input.get("wait").and_then(Value::as_bool).unwrap_or(true);
                let id = orch
                    .delegate(&self.agent, &self.task, to, spec)
                    .map_err(|e| AdapterError::Tool(e.to_string()))?;
                if !wait {
                    return Ok(ActionOutput::json(
                        json!({ "task": id, "agent": to, "state": "queued" }),
                    ));
                }
                let done = Inner::wait_children(&orch, &self.task, std::slice::from_ref(&id))
                    .await
                    .map_err(|error| AdapterError::Tool(error.to_string()))?;
                Ok(ActionOutput::json(result_json(&done[0])))
            }
            WAIT => {
                let ids: Vec<String> = input
                    .get("tasks")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                let mine: Vec<String> = ids
                    .into_iter()
                    .filter(|id| orch.is_child(&self.task, id))
                    .collect();
                if mine.is_empty() {
                    return Err(AdapterError::bad_input(
                        action,
                        "name tasks you delegated from this task",
                    ));
                }
                let done = Inner::wait_children(&orch, &self.task, &mine)
                    .await
                    .map_err(|error| AdapterError::Tool(error.to_string()))?;
                Ok(ActionOutput::json(json!({
                    "tasks": done.iter().map(result_json).collect::<Vec<_>>()
                })))
            }
            MESSAGE => {
                let to = orch
                    .agent_named(&self.agent, &self.task, s("to"))
                    .map_err(|e| AdapterError::bad_input(action, e))?;
                let to = to.as_str();
                // Text from a group member would steer a task that does not ask: work for such
                // an agent goes through `agents.start`, which asks.
                if orch.group_only(&self.agent, to) && orch.acts_alone(to) {
                    return Err(AdapterError::bad_input(
                        action,
                        format!("`{to}` acts without asking; use agents.start to ask it for work"),
                    ));
                }
                let delivered = orch
                    .send(&self.agent, to, s("text"), Some(&self.task))
                    .map_err(|e| AdapterError::Tool(e.to_string()))?;
                Ok(ActionOutput::json(json!({
                    "to": to,
                    "delivered": if delivered { "into its running task" } else { "kept for its next task" }
                })))
            }
            other => Err(AdapterError::UnknownAction(other.to_string())),
        }
    }
}

/// The tools of the person's conversation in an app that runs several agents. Built for one
/// turn, so the agents it names are the current ones.
pub(crate) struct ChatTools {
    pub orch: Orchestrator,
    /// The conversation (session id): what it schedules posts each run's result there.
    pub chat: Option<String>,
    /// The person's IANA time zone, when the client said it; else the machine's.
    pub zone: Option<String>,
}

/// Schedules the chat tools' descriptions list; the rest are counted.
const SCHEDULES_LISTED: usize = 10;

impl ChatTools {
    fn roster(&self) -> String {
        roster(&self.orch)
    }

    fn scheduling(&self) -> Scheduling<'_> {
        Scheduling {
            orch: &self.orch,
            chat: self.chat.as_deref(),
            zone: self.zone.as_deref(),
            by: None,
            auto: false,
        }
    }

    fn live(&self, name: &str) -> bool {
        self.orch
            .agent(name)
            .is_some_and(|a| a.state != AgentState::Terminated)
    }
}

/// Schedules an agent keeps at most.
const AGENT_SCHEDULES: usize = SCHEDULES_LISTED;

/// The least time between two runs of a schedule an agent sets up from a chat.
const MIN_AGENT_GAP_MS: u64 = 3_600_000;

/// The schedule tools of one chat: the person's conversation (`by: None`) or a named agent's
/// own chat (`by: Some(agent)`, ADR 0120). One code path for both. An agent's runs answer in its
/// chat as its next message, it sees and changes only its own schedules, and its new schedules
/// run on itself or on an agent made for them.
pub(crate) struct Scheduling<'a> {
    pub orch: &'a Orchestrator,
    pub chat: Option<&'a str>,
    pub zone: Option<&'a str>,
    pub by: Option<&'a str>,
    /// An agent's chat in Auto: making a schedule asks nothing (the chat's own mode).
    pub auto: bool,
}

impl Scheduling<'_> {
    fn roster(&self) -> String {
        roster(self.orch)
    }

    fn live(&self, name: &str) -> bool {
        self.orch
            .agent(name)
            .is_some_and(|a| a.state != AgentState::Terminated)
    }

    /// Whether `s` is one the asker sees: for the person's conversation, all. For an agent, those
    /// it made for itself (`reply`) and those that run on an agent made for it. Not by the chat
    /// they report to: the person's own tools may report into the same chat, and what the
    /// person made is the person's to change.
    pub(crate) fn sees(&self, s: &crate::ScheduleRecord) -> bool {
        let Some(me) = self.by else { return true };
        (s.agent == me && s.task.reply)
            || (s.own_agent
                && self
                    .orch
                    .agent(&s.agent)
                    .is_some_and(|a| a.spec.parent.as_deref() == Some(me)))
    }

    fn visible(&self) -> Vec<crate::ScheduleRecord> {
        let mut all = self.orch.schedules();
        all.retain(|s| self.sees(s));
        all
    }

    /// The schedules kept now, for the schedule tools' descriptions: when, the next run, how
    /// the last one went.
    fn scheduled(&self) -> String {
        let all = self.visible();
        if all.is_empty() {
            return "Nothing is scheduled.".into();
        }
        let mut list: Vec<String> = all
            .iter()
            .take(SCHEDULES_LISTED)
            .map(|s| {
                let state = self.orch.schedule_state(s);
                let next = match s.next_label() {
                    Some(n) => format!(", next {n}"),
                    None => String::new(),
                };
                format!(
                    "{} ({}, {}{next}, {}): {}",
                    s.id,
                    s.agent,
                    s.says,
                    state.as_str(),
                    crate::brief::clip(&s.task.objective, 60)
                )
            })
            .collect();
        if all.len() > SCHEDULES_LISTED {
            list.push(format!("and {} more", all.len() - SCHEDULES_LISTED));
        }
        format!("Scheduled: {}.", list.join("; "))
    }

    /// The zone times are read in: the person's, else the machine's.
    fn zone(&self) -> Option<String> {
        self.zone
            .map(str::to_string)
            .or_else(crate::schedule::system_zone)
    }

    /// Today's date, so "tomorrow at 9" and "next Thursday" can be resolved. The date only: a
    /// description that changed every minute would spoil the prompt cache.
    fn now_line(&self) -> String {
        let zone = self
            .zone()
            .and_then(|z| jiff::tz::TimeZone::get(&z).ok())
            .unwrap_or_else(jiff::tz::TimeZone::system);
        let now = jiff::Timestamp::now().to_zoned(zone.clone());
        format!(
            "Today: {} ({}).",
            now.strftime("%a %Y-%m-%d"),
            zone.iana_name().unwrap_or("local time")
        )
    }

    /// A free agent name for a schedule's own agent, from its objective: `recount-stock`.
    fn name_for(&self, objective: &str) -> String {
        let words: Vec<String> = objective
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() > 2)
            .take(3)
            .map(str::to_lowercase)
            .collect();
        let base = match words.join("-") {
            w if w.is_empty() => "scheduled".to_string(),
            w => w
                .chars()
                .take(24)
                .collect::<String>()
                .trim_end_matches('-')
                .to_string(),
        };
        let mut name = base.clone();
        let mut n = 2;
        while self.orch.agent(&name).is_some() {
            name = format!("{base}-{n}");
            n += 1;
        }
        name
    }

    /// Whether the asker may have `to` run a schedule: any live agent for the person; for an
    /// agent only itself. Another agent would act with its own authority (its `allow_mutate`)
    /// on a run nobody asked it for, and a run has no origin to carry a group's ask (0113).
    fn may_run_on(&self, to: &str) -> bool {
        match self.by {
            None => self.live(to),
            Some(me) => to == me && self.live(to),
        }
    }

    pub fn actions(&self, schedule: bool, unschedule: bool) -> Vec<ActionDescriptor> {
        let mut v = Vec::new();
        if schedule {
            // Standing work spends model time with nobody watching: the person agrees to it,
            // each time. No earlier "always" covers making or changing one: text the chat read
            // (a page, a mail) must never set up work that runs unattended. An agent's chat in
            // Auto is the owner's exception: it takes the chat's mode (ADR 0120).
            let d = ActionDescriptor::new(
                SCHEDULE,
                format!(
                    "do work on its own at set times (every weekday at 09:00, the 1st of each month, every 30 minutes) or once later (tomorrow at 9, in 2 hours). Each run is a new task{}. With `schedule`, change, resume or run now one of those listed under {UNSCHEDULE}. {}",
                    match (self.by, self.chat) {
                        (Some(_), _) => "; each run's answer arrives in this chat as your next message",
                        (None, Some(_)) => "; its result is posted in this chat",
                        _ => "",
                    },
                    self.now_line(),
                ),
                if self.auto {
                    RiskTier::Observe
                } else {
                    RiskTier::Mutate
                },
            )
            .volatile()
            .ends_with_short_reply()
            .with_input_schema(json!({
                "type": "object",
                "properties": {
                    "schedule": { "type": "string", "description": "id (s-…) of one to change; leave out for a new one" },
                    "to": { "type": "string", "description": "an agent; default: one of its own" },
                    "objective": { "type": "string", "description": "what each run does, complete: it does not see this chat (files, places, what to leave)" },
                    "context": { "type": "string" },
                    "at": { "type": "string", "description": "HH:MM" },
                    "days": { "type": "array", "items": { "type": "string" }, "description": "with at: mon..sun, weekdays, weekends" },
                    "date": { "type": "string", "description": "with at: YYYY-MM-DD, one run" },
                    "every_minutes": { "type": "integer", "description": "instead of at; 5 or more" },
                    "cron": { "type": "string", "description": "instead of at: minute hour day month weekday" },
                    "tz": { "type": "string", "description": "IANA zone, only if the person names another" },
                    "once": { "type": "boolean" },
                    "if_missed": { "type": "string", "enum": ["once", "skip"], "description": "a run missed while off: run once when back (default) or skip" },
                    "overlap": { "type": "boolean", "description": "run even while the last one is going" },
                    "resume": { "type": "boolean" },
                    "run_now": { "type": "boolean" }
                }
            }));
            v.push(if self.auto { d } else { d.ask_each_time() });
        }
        if unschedule {
            v.push(
                ActionDescriptor::new(
                    UNSCHEDULE,
                    format!("stop a scheduled task, or pause it to resume later. Runs already done keep their results. {}", self.scheduled()),
                    RiskTier::Observe,
                )
                .volatile()
                .ends_with_short_reply()
                .with_input_schema(json!({
                    "type": "object",
                    "required": ["schedule"],
                    "properties": {
                        "schedule": { "type": "string", "description": "its id, s-…" },
                        "pause": { "type": "boolean", "description": "keep it, run nothing until resumed (default false: stop it)" }
                    }
                })),
            );
        }
        v
    }

    /// The schedule `id`, if the asker may see it.
    fn find(&self, action: &str, id: &str) -> ark_tools::Result<()> {
        if self.visible().iter().any(|s| s.id == id) {
            Ok(())
        } else {
            Err(AdapterError::bad_input(
                action,
                format!("no schedule {id}. {}", self.scheduled()),
            ))
        }
    }

    pub async fn run(&self, action: &str, input: Value) -> ark_tools::Result<ActionOutput> {
        let s = |k: &str| input.get(k).and_then(Value::as_str).unwrap_or("").trim();
        let fail = |e: crate::OrchError| AdapterError::Tool(e.to_string());
        match action {
            SCHEDULE => {
                let bad = |e: crate::OrchError| AdapterError::bad_input(action, e.to_string());
                let id = s("schedule");
                if !id.is_empty() && self.by.is_some() {
                    self.find(action, id)?;
                }
                // A new schedule's times are the person's; a change keeps the schedule's zone
                // unless it names one.
                let when = when_of(&input, if id.is_empty() { self.zone() } else { None });
                let flag = |k: &str| input.get(k).and_then(Value::as_bool);
                let mut opts = crate::ScheduleChange {
                    if_missed: match s("if_missed") {
                        "skip" => Some(crate::IfMissed::Skip),
                        "once" => Some(crate::IfMissed::Once),
                        _ => None,
                    },
                    overlap: flag("overlap"),
                    ..Default::default()
                };
                if !id.is_empty() {
                    opts.objective = Some(s("objective").to_string()).filter(|o| !o.is_empty());
                    opts.context = input
                        .get("context")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    opts.when = when;
                    let mut rec = self.orch.change_schedule(id, opts).map_err(|e| {
                        AdapterError::bad_input(action, format!("{e}. {}", self.scheduled()))
                    })?;
                    if flag("resume") == Some(true) && rec.paused {
                        rec = self.orch.resume_schedule(id).map_err(bad)?;
                    }
                    let mut v = schedule_json(&rec);
                    if flag("run_now") == Some(true) {
                        v["run"] = json!(self.orch.run_schedule_now(id).await.map_err(bad)?);
                    }
                    v["state"] = json!(self.orch.schedule_state(&rec).as_str());
                    return Ok(ActionOutput::json(v));
                }
                let Some(when) = when else {
                    return Err(AdapterError::bad_input(
                        action,
                        "say when: at (HH:MM, with days or a date), every_minutes, or cron",
                    ));
                };
                let objective = s("objective");
                if objective.is_empty() {
                    return Err(AdapterError::bad_input(action, "an objective is required"));
                }
                let mut to = s("to").to_string();
                if !to.is_empty() && !self.may_run_on(&to) {
                    return Err(AdapterError::bad_input(
                        action,
                        match self.by {
                            Some(me) => format!(
                                "you may schedule work for yourself (`{me}`), or leave `to` out for an agent of its own"
                            ),
                            None => format!("no agent named `{to}`. {}", self.roster()),
                        },
                    ));
                }
                if let Some(me) = self.by {
                    if self.chat.is_none() {
                        return Err(AdapterError::bad_input(action, "this turn has no chat"));
                    }
                    if self.visible().len() >= AGENT_SCHEDULES {
                        return Err(AdapterError::bad_input(
                            action,
                            format!(
                                "{me} keeps at most {AGENT_SCHEDULES} schedules; stop one first. {}",
                                self.scheduled()
                            ),
                        ));
                    }
                }
                // Checked before an agent is made for it.
                let checked = when
                    .normalized()
                    .map_err(|e| AdapterError::bad_input(action, e))?;
                if checked.next_after(crate::record::now_ms()).is_none() {
                    return Err(AdapterError::bad_input(
                        action,
                        format!(
                            "{} has passed; give a time still to come",
                            checked.describe()
                        ),
                    ));
                }
                if self.by.is_some() {
                    let first = checked.next_after(crate::record::now_ms());
                    let second = first.and_then(|f| checked.next_after(f));
                    if first
                        .zip(second)
                        .is_some_and(|(a, b)| b.saturating_sub(a) < MIN_AGENT_GAP_MS)
                    {
                        return Err(AdapterError::bad_input(
                            action,
                            "runs set from a chat are at most once an hour",
                        ));
                    }
                }
                if to.is_empty() {
                    // Its own agent: what it carries from run to run is this schedule's alone.
                    to = self.name_for(objective);
                    let role = format!(
                        "does a scheduled task: {}",
                        crate::brief::clip(objective, 80)
                    );
                    // An agent's own agent has no more tools than it has and asks before it
                    // changes anything.
                    let spec = match self.by.and_then(|me| self.orch.agent(me)) {
                        Some(me) => me.spec.colleague(&to, &role, "", ""),
                        None => AgentSpec::new(&to).role(role),
                    };
                    self.orch.spawn(spec).await.map_err(fail)?;
                }
                let mut task = TaskSpec::new(objective).context(s("context"));
                if let Some(chat) = self.chat {
                    task = task.report_to(chat);
                    // Each run answers as the agent's next message, not a result card.
                    task.reply = self.by.is_some();
                }
                let own = s("to").is_empty();
                let mut rec = match self.orch.schedule_with(&to, task, when, opts) {
                    Ok(rec) => rec,
                    Err(e) => {
                        // The agent made for it ends with the refusal.
                        if own {
                            if let Err(error) = self.orch.terminate(&to).await {
                                tracing::warn!(agent = %to, %error, "could not end the agent made for a refused schedule");
                            }
                        }
                        return Err(bad(e));
                    }
                };
                if own {
                    self.orch.own_agent(&rec.id);
                    rec.own_agent = true;
                }
                Ok(ActionOutput::json(schedule_json(&rec)))
            }
            UNSCHEDULE => {
                let id = s("schedule");
                if self.by.is_some() {
                    self.find(action, id)?;
                }
                let pause = input.get("pause").and_then(Value::as_bool) == Some(true);
                let done = if pause {
                    self.orch.pause_schedule(id)
                } else {
                    self.orch.stop_schedule(id).await
                };
                let rec = done.map_err(|e| {
                    AdapterError::bad_input(action, format!("{e}. {}", self.scheduled()))
                })?;
                let mut v = schedule_json(&rec);
                v[if pause { "paused" } else { "stopped" }] = json!(true);
                Ok(ActionOutput::json(v))
            }
            other => Err(AdapterError::UnknownAction(other.to_string())),
        }
    }
}

/// `ChatTools::roster`: the live agents and their roles.
fn roster(orch: &Orchestrator) -> String {
    let live: Vec<String> = orch
        .agents()
        .into_iter()
        .filter(|a| a.state != AgentState::Terminated)
        .map(|a| match a.spec.role.trim() {
            "" => a.name,
            r => format!("{} ({r})", a.name),
        })
        .collect();
    if live.is_empty() {
        "There are no agents yet; create one first.".into()
    } else {
        format!("Agents: {}.", live.join("; "))
    }
}

#[async_trait::async_trait]
impl Adapter for ChatTools {
    fn name(&self) -> &str {
        "agents"
    }

    fn actions(&self) -> Vec<ActionDescriptor> {
        let mut v = vec![
            ActionDescriptor::new(
                DELEGATE,
                format!(
                    "hand a self-contained task to an agent and get its result. Do quick work yourself; hand off work that is long, can run in parallel, or fits an agent's role. {}",
                    self.roster()
                ),
                RiskTier::Observe,
            )
            .volatile()
            .with_input_schema(json!({
                "type": "object",
                "required": ["to", "objective"],
                "properties": {
                    "to": { "type": "string", "description": "the agent's name" },
                    "objective": { "type": "string", "description": "what the agent must achieve" },
                    "context": { "type": "string", "description": "only what it needs to know; it does not see this conversation" },
                    "item": { "type": "string", "description": "the board item (w-…) this task works on; the item then follows the task" },
                    "wait": { "type": "boolean", "description": if self.chat.is_some() { "wait for the result (default true); false returns at once, the task runs on and its result is posted in this chat" } else { "wait for the result (default true); false returns at once and the task runs on" } }
                }
            })),
            ActionDescriptor::new(
                WAIT,
                "wait for tasks handed off with wait: false and get their results",
                RiskTier::Observe,
            )
            .volatile()
            .with_input_schema(json!({
                "type": "object",
                "required": ["tasks"],
                "properties": { "tasks": { "type": "array", "items": { "type": "string" } } }
            })),
            ActionDescriptor::new(
                CREATE,
                "create a persistent specialist with its own context; changes still need approval",
                RiskTier::Observe,
            )
            .volatile()
            .with_input_schema(json!({
                "type": "object",
                "required": ["name", "role"],
                "properties": {
                    "name": { "type": "string", "description": "short, lowercase, e.g. reviewer" },
                    "role": { "type": "string", "description": "one line on what it does" },
                    "instructions": { "type": "string", "description": "how it works, as a system prompt" },
                    "model": { "type": "string", "description": "provider:model (account allowed); omitted uses default" },
                    "reasoning": { "type": "string", "description": "supported effort for the selected or default model" },
                    "contacts": { "type": "array", "items": { "type": "string" }, "description": "permitted names, including future agents; connect both directions for discussion" },
                    "skills": { "type": "array", "items": { "type": "string" }, "description": "allowed skill names from SKILLS" },
                    "capabilities": {
                        "type": "array", "items": { "type": "string" },
                        "description": format!("capabilities this role needs; omitted: all; choose: {}", self.orch.capabilities().join(", "))
                    }
                }
            })),
        ];
        v.extend(self.scheduling().actions(true, true));
        v
    }

    /// The work a stopped turn of this chat handed off, for the turn that continues it.
    fn continued(&self, works: &[String]) -> Vec<String> {
        self.orch
            .tasks_of_work(works)
            .into_iter()
            .map(|t| {
                let objective = crate::brief::clip(&t.objective, 100);
                let state = if t.state.is_final() {
                    match (&t.result, &t.error) {
                        (_, Some(e)) => {
                            format!("{}: {}", t.state.as_str(), crate::brief::clip(e, 120))
                        }
                        (Some(r), _) => format!("finished: {}", crate::brief::clip(r, 160)),
                        _ => t.state.as_str().to_string(),
                    }
                } else {
                    format!(
                        "{}, still under way; its result is posted in this chat",
                        t.state.as_str()
                    )
                };
                format!(
                    "handed off to {} (task {}): {objective} ({state})",
                    t.owner, t.id
                )
            })
            .collect()
    }

    async fn run(
        &self,
        action: &str,
        input: Value,
        ctx: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        let s = |k: &str| input.get(k).and_then(Value::as_str).unwrap_or("").trim();
        let fail = |e: crate::OrchError| AdapterError::Tool(e.to_string());
        match action {
            DELEGATE => {
                let live: Vec<(String, String)> = self
                    .orch
                    .agents()
                    .into_iter()
                    .filter(|a| a.state != AgentState::Terminated)
                    .map(|a| (a.name, a.spec.title))
                    .collect();
                let to = crate::names::resolve(s("to"), &live)
                    .map_err(|e| AdapterError::bad_input(action, e))?;
                let to = to.as_str();
                let mut spec = TaskSpec::new(s("objective")).context(s("context"));
                if spec.objective.is_empty() {
                    return Err(AdapterError::bad_input(action, "an objective is required"));
                }
                if !s("item").is_empty() {
                    spec = spec.item(s("item"));
                }
                // Its result comes back to this chat even when this turn is gone (a restart).
                if let Some(chat) = &self.chat {
                    spec = spec.report_to(chat);
                }
                // A turn that stopped handed work to this agent and it is still under way: the
                // turn continuing it takes that up, once, instead of handing it off twice. The
                // model sees what it was; a second hand-off to the agent in this turn is new.
                let taken = ctx.work.as_deref().and_then(|w| {
                    self.orch
                        .take_up(to, &ctx.continues, w)
                        .map(|t| (t.id, t.objective))
                });
                if let Some(w) = &ctx.work {
                    spec = spec.work(w);
                }
                let (id, joined) = match taken {
                    Some((id, objective)) => (id, Some(objective)),
                    None => (
                        self.orch
                            .submit(to, spec)
                            .map_err(|e| AdapterError::bad_input(action, e.to_string()))?,
                        None,
                    ),
                };
                // In other words than before, it may be other work: say what is under way, and
                // do not wait for it. The model hands off again if it wants something else.
                let wait = input.get("wait").and_then(Value::as_bool).unwrap_or(true)
                    && joined.as_deref().is_none_or(|o| o == s("objective"));
                let mark = |mut v: Value| {
                    if let Some(o) = &joined {
                        v["taken_up"] = json!(format!(
                            "this is the hand-off made before the interruption, still under way: {o}. Hand off again only for different work"
                        ));
                    }
                    v
                };
                if !wait {
                    let state = self.orch.task(&id).map(|t| t.state.as_str().to_string());
                    let mut v = json!({ "task": id, "agent": to, "state": state.unwrap_or_else(|| "queued".into()) });
                    if self.chat.is_some() {
                        v["result"] = json!("posted in this chat when it ends");
                    }
                    return Ok(ActionOutput::json(mark(v)));
                }
                let done = self
                    .orch
                    .wait_in(&id, self.chat.as_deref())
                    .await
                    .map_err(fail)?;
                Ok(ActionOutput::json(mark(result_json(&done))))
            }
            WAIT => {
                let ids: Vec<&str> = input
                    .get("tasks")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default();
                if ids.is_empty() {
                    return Err(AdapterError::bad_input(
                        action,
                        "name the tasks to wait for",
                    ));
                }
                let mut done = Vec::new();
                for id in ids {
                    let rec = self
                        .orch
                        .wait_in(id, self.chat.as_deref())
                        .await
                        .map_err(fail)?;
                    done.push(result_json(&rec));
                }
                Ok(ActionOutput::json(json!({ "tasks": done })))
            }
            CREATE => {
                let name = s("name").to_lowercase().replace(' ', "-");
                if self.live(&name) {
                    return Err(AdapterError::bad_input(
                        action,
                        format!("`{name}` exists; hand it the task with {DELEGATE}"),
                    ));
                }
                let skills: Vec<String> = input
                    .get("skills")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                let known = self.orch.capabilities();
                let mut tools: Vec<String> = Vec::new();
                for c in input
                    .get("capabilities")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                {
                    let c = c.trim().trim_end_matches(".*").to_lowercase();
                    if !known.contains(&c) {
                        return Err(AdapterError::bad_input(
                            action,
                            format!("no capability `{c}`; one of: {}", known.join(", ")),
                        ));
                    }
                    tools.push(format!("{c}.*"));
                }
                let contacts = input
                    .get("contacts")
                    .map(|value| {
                        serde_json::from_value::<Vec<String>>(value.clone()).map_err(|_| {
                            AdapterError::bad_input(
                                action,
                                "contacts must be a list of agent names",
                            )
                        })
                    })
                    .transpose()?
                    .unwrap_or_default();
                if contacts
                    .iter()
                    .any(|name| !crate::orchestrator::valid_name(name))
                {
                    return Err(AdapterError::bad_input(
                        action,
                        "contacts must be valid agent names",
                    ));
                }
                let mut spec = AgentSpec::new(&name)
                    .role(s("role"))
                    .instructions(s("instructions"))
                    .skills(skills)
                    .delegates_to(contacts);
                if !tools.is_empty() {
                    spec = spec.tools(tools);
                }
                let a = self
                    .orch
                    .create_agent(
                        spec,
                        crate::execution::ExecutionSelection::parse(&input).map_err(fail)?,
                    )
                    .await
                    .map_err(fail)?;
                Ok(ActionOutput::json(
                    json!({ "agent": a.name, "role": a.spec.role, "tools": a.spec.tools }),
                ))
            }
            SCHEDULE | UNSCHEDULE => self.scheduling().run(action, input).await,
            other => Err(AdapterError::UnknownAction(other.to_string())),
        }
    }
}

/// The project's boards for the person's conversation: read them, add items, change them. Its
/// changes are the chat's (`by: chat`).
pub(crate) struct BoardTools {
    pub orch: Orchestrator,
}

#[async_trait::async_trait]
impl Adapter for BoardTools {
    fn name(&self) -> &str {
        "board"
    }

    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![
            ActionDescriptor::new(
                BOARD_LIST,
                format!(
                    "read the project's boards, one per goal: the work planned, in progress and done, kept across chats and sessions. Read them before planning or continuing project work. {}",
                    board::summary(&self.orch.boards(), &self.orch.board())
                ),
                RiskTier::Observe,
            )
            .volatile()
            .with_input_schema(json!({ "type": "object", "properties": {} })),
            ActionDescriptor::new(
                BOARD_ADD,
                "add items to the board of one goal: pieces of work worth tracking (a deliverable, a step that takes a task), not every small action. A new goal makes a new board, which the person sees beside the chat. Hand an item to an agent with agents.delegate and its item id",
                RiskTier::Observe,
            )
            .with_input_schema(json!({
                "type": "object",
                "required": ["board", "items"],
                "properties": {
                    "board": { "type": "string", "description": "the goal this work is for, in a few words: an existing board's goal or id (b-…), or a new goal" },
                    "items": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "required": ["title"],
                            "properties": {
                                "title": { "type": "string", "description": "short, what is done when it is done" },
                                "note": { "type": "string" },
                                "owner": { "type": "string", "description": "an agent, or user for the person" }
                            }
                        }
                    }
                }
            })),
            ActionDescriptor::new(
                BOARD_UPDATE,
                "change a board item: its state, what blocks it (with owner user when only the person can unblock it; empty clears it), owner, note or title",
                RiskTier::Observe,
            )
            .with_input_schema(json!({
                "type": "object",
                "required": ["item"],
                "properties": {
                    "item": { "type": "string", "description": "w-…" },
                    "state": { "type": "string", "enum": ["todo", "doing", "done"] },
                    "blocked": { "type": "string" },
                    "owner": { "type": "string" },
                    "note": { "type": "string" },
                    "title": { "type": "string" }
                }
            })),
        ]
    }

    async fn run(
        &self,
        action: &str,
        input: Value,
        ctx: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        let opt = |v: &Value, k: &str| {
            v.get(k)
                .and_then(Value::as_str)
                .map(|x| x.trim().to_string())
        };
        let bad = |e: crate::OrchError| AdapterError::bad_input(action, e.to_string());
        match action {
            BOARD_LIST => {
                let all = self.orch.board();
                let boards = self.orch.boards();
                let mut room = BOARD_LISTED;
                let listed: Vec<Value> = boards
                    .iter()
                    .take(board::SUMMARY_BOARDS)
                    .map(|b| {
                        let open: Vec<&WorkItem> = all
                            .iter()
                            .filter(|w| w.board == b.id && w.state != WorkState::Done)
                            .collect();
                        let done: Vec<&WorkItem> = all
                            .iter()
                            .filter(|w| w.board == b.id && w.state == WorkState::Done)
                            .collect();
                        let shown = open.len().min(room);
                        room -= shown;
                        json!({
                            "board": b.id,
                            "goal": b.goal,
                            "items": open.iter().take(shown).map(|w| item_json(w)).collect::<Vec<_>>(),
                            "more_open": open.len() - shown,
                            "done": done.iter().take(DONE_LISTED).map(|w| item_json(w)).collect::<Vec<_>>(),
                            "more_done": done.len().saturating_sub(DONE_LISTED),
                        })
                    })
                    .collect();
                Ok(ActionOutput::json(json!({
                    "summary": board::summary(&boards, &all),
                    "boards": listed,
                })))
            }
            BOARD_ADD => {
                let items = input
                    .get("items")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                if items.is_empty() {
                    return Err(AdapterError::bad_input(action, "name at least one item"));
                }
                let named = opt(&input, "board").unwrap_or_default();
                if named.is_empty() {
                    return Err(AdapterError::bad_input(
                        action,
                        "name the goal this work is for in `board`: an existing board's goal or id, or a new goal",
                    ));
                }
                let board = match self.orch.boards().into_iter().find(|b| b.id == named) {
                    Some(b) => b,
                    // An id that names no board is a mistake, not a goal.
                    None if is_board_id(&named) => {
                        return Err(AdapterError::bad_input(
                            action,
                            format!(
                                "no board `{named}`; {}",
                                board::summary(&self.orch.boards(), &self.orch.board())
                            ),
                        ))
                    }
                    None => self
                        .orch
                        .add_board(CHAT, &named, chat_of(ctx))
                        .map_err(bad)?,
                };
                let mut added = Vec::new();
                for it in &items {
                    let title = opt(it, "title").unwrap_or_default();
                    let change = WorkChange {
                        note: opt(it, "note").filter(|n| !n.is_empty()),
                        owner: opt(it, "owner").filter(|n| !n.is_empty()),
                        board: Some(board.id.clone()),
                        ..Default::default()
                    };
                    added.push(item_json(
                        &self.orch.add_work(CHAT, &title, change).map_err(bad)?,
                    ));
                }
                Ok(ActionOutput::json(
                    json!({ "board": board.id, "goal": board.goal, "added": added }),
                ))
            }
            BOARD_UPDATE => {
                let id = opt(&input, "item").unwrap_or_default();
                let state = match opt(&input, "state").filter(|x| !x.is_empty()) {
                    Some(x) => Some(WorkState::parse(&x).ok_or_else(|| {
                        AdapterError::bad_input(action, "state is todo, doing or done")
                    })?),
                    None => None,
                };
                let change = WorkChange {
                    title: opt(&input, "title").filter(|n| !n.is_empty()),
                    note: opt(&input, "note"),
                    state,
                    owner: opt(&input, "owner"),
                    blocked: opt(&input, "blocked"),
                    before: None,
                    board: None,
                };
                let w = self.orch.change_work(CHAT, &id, change).map_err(bad)?;
                Ok(ActionOutput::json(json!({ "updated": item_json(&w) })))
            }
            other => Err(AdapterError::UnknownAction(other.to_string())),
        }
    }
}

/// The chat a turn's call is made in: its work is `session:<chat>/<turn>`.
fn chat_of(ctx: &RunCtx<'_>) -> Option<String> {
    let work = ctx.work.as_deref()?.strip_prefix("session:")?;
    let chat = work.rsplit_once('/').map_or(work, |(chat, _)| chat);
    (!chat.is_empty()).then(|| chat.to_string())
}

/// `b-` and eight hex digits, as [`board::Board`] ids are made.
fn is_board_id(s: &str) -> bool {
    s.strip_prefix("b-")
        .is_some_and(|h| h.len() == 8 && h.bytes().all(|c| c.is_ascii_hexdigit()))
}

/// A board's goal and id as the model reads them: `"Launch the site" (b-1a2b3c4d)`.
fn board_name(v: &Value) -> String {
    let g = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("");
    format!("\"{}\" ({})", g("goal"), g("board"))
}

fn item_json(w: &WorkItem) -> Value {
    let mut v = json!({ "item": w.id, "title": w.title, "state": w.state.as_str() });
    if let Some(o) = &w.owner {
        v["owner"] = json!(o);
    }
    if let Some(b) = &w.blocked {
        v["blocked"] = json!(b);
    }
    if !w.note.is_empty() {
        v["note"] = json!(crate::brief::clip(&w.note, 300));
    }
    v
}

/// One line per item in an evidence card: `w-1a2b3c4d [doing, writer] Title (blocked: …)`.
fn item_line(v: &Value) -> String {
    let g = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("");
    let mut s = format!("{} [{}", g("item"), g("state"));
    if !g("owner").is_empty() {
        s.push_str(&format!(", {}", g("owner")));
    }
    s.push_str(&format!("] {}", g("title")));
    if !g("blocked").is_empty() {
        s.push_str(&format!(" (blocked: {})", g("blocked")));
    }
    s
}

/// Evidence cards for `board.*` results: one line per item, the item ids as identifiers.
pub(crate) fn board_distiller() -> Distiller {
    Arc::new(|action: &str, out: &Value| {
        let lines = |key: &str| -> Vec<Value> {
            match out.get(key) {
                Some(Value::Array(a)) => a.clone(),
                Some(v @ Value::Object(_)) => vec![v.clone()],
                _ => Vec::new(),
            }
        };
        let ids = |items: &[Value]| -> Vec<String> {
            items
                .iter()
                .filter_map(|v| v.get("item").and_then(Value::as_str).map(str::to_string))
                .collect()
        };
        match action {
            BOARD_LIST => {
                let mut s = out
                    .get("summary")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let mut all = Vec::new();
                for b in lines("boards") {
                    let part = |key: &str| -> Vec<Value> {
                        b.get(key)
                            .and_then(Value::as_array)
                            .cloned()
                            .unwrap_or_default()
                    };
                    let (open, done) = (part("items"), part("done"));
                    s.push_str(&format!("\n\nBoard {}:", board_name(&b)));
                    for v in &open {
                        s.push_str(&format!("\n{}", item_line(v)));
                    }
                    if let Some(n) = b
                        .get("more_open")
                        .and_then(Value::as_u64)
                        .filter(|n| *n > 0)
                    {
                        s.push_str(&format!("\n({n} more open)"));
                    }
                    for v in &done {
                        s.push_str(&format!("\n{}", item_line(v)));
                    }
                    all.extend(b.get("board").and_then(Value::as_str).map(str::to_string));
                    all.extend(ids(&open));
                    all.extend(ids(&done));
                }
                Some((s, all))
            }
            BOARD_ADD => {
                let added = lines("added");
                let s = added.iter().map(item_line).collect::<Vec<_>>().join("\n");
                let n = match added.len() {
                    1 => "1 item".to_string(),
                    n => format!("{n} items"),
                };
                let mut all: Vec<String> = out
                    .get("board")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .into_iter()
                    .collect();
                all.extend(ids(&added));
                Some((
                    format!("added {n} to the board {}:\n{s}", board_name(out)),
                    all,
                ))
            }
            BOARD_UPDATE => {
                let v = lines("updated");
                Some((
                    format!(
                        "board item now: {}",
                        v.first().map(item_line).unwrap_or_default()
                    ),
                    ids(&v),
                ))
            }
            _ => None,
        }
    })
}

fn schedule_json(s: &crate::ScheduleRecord) -> Value {
    json!({
        "schedule": s.id,
        "agent": s.agent,
        "objective": s.task.objective,
        "when": s.says,
        "tz": s.when.tz,
        "next": s.next_label(),
        "paused": s.paused,
    })
}

/// The `When` an `agents.schedule` input gives, if it gives one; times of day in `zone` unless
/// it names its own.
fn when_of(input: &Value, zone: Option<String>) -> Option<crate::When> {
    let s = |k: &str| {
        input
            .get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    };
    let w = crate::When {
        every_minutes: input
            .get("every_minutes")
            .and_then(Value::as_u64)
            .map(|m| m.min(u32::MAX as u64) as u32),
        at: s("at"),
        days: input
            .get("days")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        date: s("date"),
        cron: s("cron"),
        tz: s("tz").or(zone),
        once: input.get("once").and_then(Value::as_bool).unwrap_or(false),
    };
    (w.every_minutes.is_some() || w.at.is_some() || w.cron.is_some()).then_some(w)
}

fn result_json(t: &TaskRecord) -> Value {
    json!({
        "task": t.id,
        "agent": t.owner,
        "state": t.state.as_str(),
        "result": t.result,
        "error": t.error,
        "artifacts": t.outputs,
        "tokens": t.usage.tokens(),
    })
}

/// Evidence cards for `agents.*` results: the delegated result itself, clipped, with the
/// task and artifact ids as identifiers.
pub(crate) fn distiller() -> Distiller {
    Arc::new(|action: &str, out: &Value| {
        let one = |t: &Value, path: &str| -> (String, Vec<String>) {
            let g = |k: &str| t.get(k).and_then(Value::as_str).unwrap_or("");
            let mut ids = vec![g("task").to_string()];
            let mut line = format!("{} {} task {}", g("agent"), g("state"), g("task"));
            if !g("error").is_empty() {
                line.push_str(&format!(" (error: {})", g("error")));
            }
            if !g("taken_up").is_empty() {
                line.push_str(&format!(" ({})", g("taken_up")));
            }
            if !g("result").is_empty() {
                line.push_str(&format!(
                    ": {}",
                    crate::brief::clip(g("result"), RESULT_CHARS)
                ));
                let n = g("result").chars().count();
                if n > RESULT_CHARS {
                    // Say how to read the rest: a delegator that misses it works from half a result.
                    line.push_str(&format!(
                        "\n  (clipped: {RESULT_CHARS} of {n} characters; get_evidence on this invocation with path \"{path}\" reads all of it)"
                    ));
                }
            }
            for a in t
                .get("artifacts")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let id = a.get("id").and_then(Value::as_str).unwrap_or("");
                let name = a.get("name").and_then(Value::as_str).unwrap_or("");
                line.push_str(&format!(
                    "\n  produced art:{} {name}",
                    crate::brief::short_id(id)
                ));
                ids.push(id.to_string());
            }
            (line, ids)
        };
        match action {
            DELEGATE => Some(one(out, "result")),
            START => Some((
                format!(
                    "started independent turn {} for {}; {}",
                    out.get("task").and_then(Value::as_str).unwrap_or(""),
                    out.get("agent").and_then(Value::as_str).unwrap_or(""),
                    if out.get("comes_back").and_then(Value::as_bool) == Some(true) {
                        "its result comes back to this chat when it finishes, do not wait"
                    } else {
                        "do not wait"
                    }
                ),
                Vec::new(),
            )),
            WAIT => {
                let mut lines = Vec::new();
                let mut ids = Vec::new();
                for (n, t) in out
                    .get("tasks")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .enumerate()
                {
                    let (l, i) = one(t, &format!("tasks.{n}.result"));
                    lines.push(l);
                    ids.extend(i);
                }
                Some((lines.join("\n"), ids))
            }
            CREATE => Some((
                format!(
                    "created agent {}: {}",
                    out.get("agent").and_then(Value::as_str).unwrap_or(""),
                    out.get("role").and_then(Value::as_str).unwrap_or("")
                ),
                Vec::new(),
            )),
            SCHEDULE | UNSCHEDULE => {
                let g = |k: &str| out.get(k).and_then(Value::as_str).unwrap_or("");
                let yes = |k: &str| out.get(k).and_then(Value::as_bool) == Some(true);
                let verb = if yes("stopped") {
                    "stopped"
                } else if yes("paused") {
                    "paused"
                } else {
                    "scheduled"
                };
                let mut card = format!(
                    "{verb} {}: {} runs \"{}\" {}",
                    g("schedule"),
                    g("agent"),
                    crate::brief::clip(g("objective"), 120),
                    g("when")
                );
                if !g("next").is_empty() && verb == "scheduled" {
                    card.push_str(&format!(", next {}", g("next")));
                }
                if !g("run").is_empty() {
                    card.push_str(&format!("; started run {}", g("run")));
                }
                Some((card, vec![g("schedule").to_string()]))
            }
            MESSAGE => Some((
                format!(
                    "message to {} {}",
                    out.get("to").and_then(Value::as_str).unwrap_or(""),
                    out.get("delivered").and_then(Value::as_str).unwrap_or("")
                ),
                Vec::new(),
            )),
            _ => None,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clipped_result_says_how_to_read_the_rest() {
        let d = distiller();
        let long = "x".repeat(RESULT_CHARS + 50);
        let (card, ids) = d(
            DELEGATE,
            &json!({ "task": "t-1", "agent": "w", "state": "done", "result": long }),
        )
        .unwrap();
        assert!(card.contains("path \"result\" reads all of it"), "{card}");
        assert_eq!(ids, vec!["t-1"]);
        let (card, _) = d(
            WAIT,
            &json!({ "tasks": [
                { "task": "t-1", "agent": "w", "state": "done", "result": "short" },
                { "task": "t-2", "agent": "w", "state": "done", "result": "y".repeat(RESULT_CHARS + 1) }
            ] }),
        )
        .unwrap();
        assert!(card.contains("path \"tasks.1.result\""), "{card}");
        assert!(!card.contains("tasks.0.result"));
    }
}
