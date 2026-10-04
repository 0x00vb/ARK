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
//! the chat decides what becomes a task.
//!
//! The calls are ordinary actions: each becomes an invocation in the delegating agent's
//! store and an evidence card in its context, so a delegation is traced like any tool call
//! and its result is cited like any evidence.

use std::sync::{Arc, Weak};

use ark_core::{ActionDescriptor, RiskTier};
use ark_runtime::Distiller;
use ark_tools::{ActionOutput, Adapter, AdapterError, RunCtx};
use serde_json::{json, Value};

use crate::orchestrator::Inner;
use crate::record::{AgentState, TaskRecord};
use crate::spec::{AgentSpec, TaskSpec};
use crate::Orchestrator;

pub const DELEGATE: &str = "agents.delegate";
pub const WAIT: &str = "agents.wait";
pub const MESSAGE: &str = "agents.message";
pub const CREATE: &str = "agents.create";
pub const SCHEDULE: &str = "agents.schedule";
pub const UNSCHEDULE: &str = "agents.unschedule";

/// Characters of a delegated task's result in the delegator's evidence card. The full result
/// stays in the invocation (`get_evidence`).
pub const RESULT_CHARS: usize = 2400;

pub(crate) struct AgentTools {
    pub orch: Weak<Inner>,
    pub agent: String,
    pub task: String,
    /// Agents it may delegate to: (name, role).
    pub delegates: Vec<(String, String)>,
    /// Agents it may message.
    pub contacts: Vec<String>,
}

#[async_trait::async_trait]
impl Adapter for AgentTools {
    fn name(&self) -> &str {
        "agents"
    }

    fn actions(&self) -> Vec<ActionDescriptor> {
        let mut v = Vec::new();
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
        if !self.contacts.is_empty() {
            v.push(
                ActionDescriptor::new(
                    MESSAGE,
                    "send a short message to another agent",
                    RiskTier::Observe,
                )
                .volatile()
                .with_input_schema(json!({
                    "type": "object",
                    "required": ["to", "text"],
                    "properties": {
                        "to": { "type": "string", "enum": self.contacts },
                        "text": { "type": "string" }
                    }
                })),
            );
        }
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
            DELEGATE => {
                let to = s("to");
                if !self.delegates.iter().any(|(n, _)| n == to) {
                    return Err(AdapterError::bad_input(
                        action,
                        format!("you may not delegate to `{to}`"),
                    ));
                }
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
                let done = Inner::wait_children(&orch, &self.task, std::slice::from_ref(&id)).await;
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
                let done = Inner::wait_children(&orch, &self.task, &mine).await;
                Ok(ActionOutput::json(json!({
                    "tasks": done.iter().map(result_json).collect::<Vec<_>>()
                })))
            }
            MESSAGE => {
                let to = s("to");
                if !self.contacts.iter().any(|c| c == to) {
                    return Err(AdapterError::bad_input(
                        action,
                        format!("you may not message `{to}`"),
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
}

impl ChatTools {
    fn roster(&self) -> String {
        let live: Vec<String> = self
            .orch
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

    /// The schedules running now, for the schedule tools' descriptions.
    fn scheduled(&self) -> String {
        let all = self.orch.schedules();
        if all.is_empty() {
            return "Nothing is scheduled.".into();
        }
        let list: Vec<String> = all
            .iter()
            .map(|s| {
                format!(
                    "{} ({}, {}: {})",
                    s.id,
                    s.agent,
                    s.says,
                    crate::brief::clip(&s.task.objective, 60)
                )
            })
            .collect();
        format!("Scheduled: {}.", list.join("; "))
    }

    fn live(&self, name: &str) -> bool {
        self.orch
            .agent(name)
            .is_some_and(|a| a.state != AgentState::Terminated)
    }
}

#[async_trait::async_trait]
impl Adapter for ChatTools {
    fn name(&self) -> &str {
        "agents"
    }

    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![
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
                    "wait": { "type": "boolean", "description": "wait for the result (default true); false returns at once and the task runs on" }
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
                "create an agent when the work needs a role no agent has. It keeps its own context between tasks; actions that change anything still ask the person",
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
                    "skills": { "type": "array", "items": { "type": "string" }, "description": "skills it may read, by name from the SKILLS list" },
                    "capabilities": {
                        "type": "array", "items": { "type": "string" },
                        "description": format!("what it may use, only what its role needs; default: all. One of: {}", self.orch.capabilities().join(", "))
                    }
                }
            })),
            // Standing work spends model time with nobody watching: the person agrees to it.
            ActionDescriptor::new(
                SCHEDULE,
                format!(
                    "have an agent run a task on its own at set times (every weekday at 09:00, every 30 minutes) or once later (in 2 hours, at 18:00). Each run is a new task; it runs whether or not this chat is open. {}",
                    self.roster()
                ),
                RiskTier::Mutate,
            )
            .volatile()
            .with_input_schema(json!({
                "type": "object",
                "required": ["to", "objective"],
                "properties": {
                    "to": { "type": "string", "description": "the agent's name" },
                    "objective": { "type": "string", "description": "what each run must achieve" },
                    "context": { "type": "string", "description": "only what it needs to know; it does not see this conversation" },
                    "at": { "type": "string", "description": "local time of day, HH:MM (24-hour)" },
                    "days": { "type": "array", "items": { "type": "string" }, "description": "with at: mon .. sun, weekdays or weekends; default every day" },
                    "every_minutes": { "type": "integer", "description": "instead of at: run every this many minutes" },
                    "once": { "type": "boolean", "description": "run one time (at the next `at`, or after `every_minutes`) and stop" }
                }
            })),
            ActionDescriptor::new(
                UNSCHEDULE,
                format!("stop a scheduled task. {}", self.scheduled()),
                RiskTier::Observe,
            )
            .volatile()
            .with_input_schema(json!({
                "type": "object",
                "required": ["schedule"],
                "properties": { "schedule": { "type": "string", "description": "its id, s-…" } }
            })),
        ]
    }

    async fn run(
        &self,
        action: &str,
        input: Value,
        _ctx: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        let s = |k: &str| input.get(k).and_then(Value::as_str).unwrap_or("").trim();
        let fail = |e: crate::OrchError| AdapterError::Tool(e.to_string());
        match action {
            DELEGATE => {
                let to = s("to");
                if !self.live(to) {
                    return Err(AdapterError::bad_input(
                        action,
                        format!("no agent named `{to}`. {}", self.roster()),
                    ));
                }
                let spec = TaskSpec::new(s("objective")).context(s("context"));
                if spec.objective.is_empty() {
                    return Err(AdapterError::bad_input(action, "an objective is required"));
                }
                let id = self.orch.submit(to, spec).map_err(fail)?;
                if !input.get("wait").and_then(Value::as_bool).unwrap_or(true) {
                    return Ok(ActionOutput::json(
                        json!({ "task": id, "agent": to, "state": "queued" }),
                    ));
                }
                let done = self.orch.wait(&id).await.map_err(fail)?;
                Ok(ActionOutput::json(result_json(&done)))
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
                    done.push(result_json(&self.orch.wait(id).await.map_err(fail)?));
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
                let mut spec = AgentSpec::new(&name)
                    .role(s("role"))
                    .instructions(s("instructions"))
                    .skills(skills);
                if !tools.is_empty() {
                    spec = spec.tools(tools);
                }
                let a = self.orch.spawn(spec).await.map_err(fail)?;
                Ok(ActionOutput::json(
                    json!({ "agent": a.name, "role": a.spec.role, "tools": a.spec.tools }),
                ))
            }
            SCHEDULE => {
                let to = s("to");
                if !self.live(to) {
                    return Err(AdapterError::bad_input(
                        action,
                        format!("no agent named `{to}`. {}", self.roster()),
                    ));
                }
                let when = crate::schedule::When {
                    every_minutes: input
                        .get("every_minutes")
                        .and_then(Value::as_u64)
                        .map(|m| m.min(u32::MAX as u64) as u32),
                    at: Some(s("at")).filter(|a| !a.is_empty()).map(str::to_string),
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
                    once: input.get("once").and_then(Value::as_bool).unwrap_or(false),
                };
                let task = TaskSpec::new(s("objective")).context(s("context"));
                let rec = self
                    .orch
                    .schedule(to, task, when)
                    .map_err(|e| AdapterError::bad_input(action, e.to_string()))?;
                Ok(ActionOutput::json(schedule_json(&rec)))
            }
            UNSCHEDULE => {
                let rec = self.orch.unschedule(s("schedule")).map_err(|e| {
                    AdapterError::bad_input(action, format!("{e}. {}", self.scheduled()))
                })?;
                let mut v = schedule_json(&rec);
                v["stopped"] = json!(true);
                Ok(ActionOutput::json(v))
            }
            other => Err(AdapterError::UnknownAction(other.to_string())),
        }
    }
}

fn schedule_json(s: &crate::ScheduleRecord) -> Value {
    json!({
        "schedule": s.id,
        "agent": s.agent,
        "objective": s.task.objective,
        "when": s.says,
        "next_ms": s.next_ms,
    })
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
                let verb = if action == UNSCHEDULE {
                    "stopped"
                } else {
                    "scheduled"
                };
                Some((
                    format!(
                        "{verb} {}: {} runs \"{}\" {}",
                        g("schedule"),
                        g("agent"),
                        crate::brief::clip(g("objective"), 120),
                        g("when")
                    ),
                    vec![g("schedule").to_string()],
                ))
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
