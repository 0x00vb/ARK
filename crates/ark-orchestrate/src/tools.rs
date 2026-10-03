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
//! The calls are ordinary actions: each becomes an invocation in the delegating agent's
//! store and an evidence card in its context, so a delegation is traced like any tool call
//! and its result is cited like any evidence.

use std::sync::{Arc, Weak};

use ark_core::{ActionDescriptor, RiskTier};
use ark_runtime::Distiller;
use ark_tools::{ActionOutput, Adapter, AdapterError, RunCtx};
use serde_json::{json, Value};

use crate::orchestrator::Inner;
use crate::record::TaskRecord;
use crate::spec::TaskSpec;

pub const DELEGATE: &str = "agents.delegate";
pub const WAIT: &str = "agents.wait";
pub const MESSAGE: &str = "agents.message";

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
