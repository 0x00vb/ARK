//! What an agent receives for a task: the objective, the context its delegator chose to
//! give, the artifacts it may use, the results of the tasks it runs after, and messages
//! waiting for it. Never the delegator's conversation, working state or tool output.

use std::fmt::Write as _;

use crate::record::{Mail, Origin, TaskRecord};

/// Characters of a prerequisite task's result put in a brief. The whole result stays on
/// the task and in the store.
pub const DEP_RESULT_CHARS: usize = 1200;

pub struct Brief<'a> {
    pub task: &'a TaskRecord,
    /// Results of the tasks it runs after, in order.
    pub deps: Vec<&'a TaskRecord>,
    /// Tasks it delegated in an earlier attempt (shown when it resumes).
    pub children: Vec<&'a TaskRecord>,
    pub inbox: &'a [Mail],
    /// It was stopped by a suspend and runs again.
    pub resumed: bool,
}

impl Brief<'_> {
    pub fn render(&self) -> String {
        let t = self.task;
        let mut s = String::new();
        match &t.origin {
            Origin::Agent { agent, task } => {
                let _ = writeln!(
                    s,
                    "Task {} delegated by {agent} (from its task {task}).",
                    t.id
                );
            }
            Origin::External => {
                let _ = writeln!(s, "Task {}.", t.id);
            }
        }
        if self.resumed {
            s.push_str(
                "It was interrupted before it finished. Your working state holds what you found; continue from there.\n",
            );
            for c in &self.children {
                let what = match (&c.result, c.state.is_final()) {
                    (Some(r), _) => clip(r, DEP_RESULT_CHARS),
                    (None, true) => c.error.clone().unwrap_or_default(),
                    (None, false) => "not finished; agents.wait gets its result".into(),
                };
                let _ = writeln!(
                    s,
                    "- you delegated {} to {} ({}): {what}",
                    c.id,
                    c.owner,
                    c.state.as_str()
                );
            }
        }
        let _ = write!(s, "\n== OBJECTIVE ==\n{}\n", t.objective.trim());
        if !t.context.trim().is_empty() {
            let from = match &t.origin {
                Origin::Agent { agent, .. } => agent.as_str(),
                Origin::External => "the requester",
            };
            let _ = write!(s, "\n== CONTEXT FROM {} ==\n{}\n", from, t.context.trim());
        }
        if !t.inputs.is_empty() || !self.deps.is_empty() {
            s.push_str("\n== INPUTS ==\n");
            for a in &t.inputs {
                let _ = writeln!(s, "- art:{} {} ({})", short_id(&a.id), a.name, a.kind);
            }
            for d in &self.deps {
                let result = d.result.as_deref().unwrap_or("(no result)");
                let _ = writeln!(
                    s,
                    "- result of {} ({}): {}",
                    d.id,
                    d.owner,
                    clip(result, DEP_RESULT_CHARS)
                );
                for a in &d.outputs {
                    let _ = writeln!(s, "  - art:{} {} ({})", short_id(&a.id), a.name, a.kind);
                }
            }
        }
        if !self.inbox.is_empty() {
            s.push_str("\n== MESSAGES ==\n");
            for m in self.inbox {
                let _ = writeln!(s, "- from {}: {}", m.from, clip(&m.text, 600));
            }
        }
        let b = &t.budget;
        let mut limits = Vec::new();
        if let Some(n) = b.max_steps {
            limits.push(format!("{n} steps"));
        }
        if let Some(n) = b.max_tokens {
            limits.push(format!("about {n} tokens"));
        }
        if let Some(n) = b.max_delegations {
            limits.push(format!("{n} delegations"));
        }
        if !limits.is_empty() {
            let _ = write!(s, "\n== LIMITS ==\n{}\n", limits.join(", "));
        }
        if matches!(t.origin, Origin::Agent { .. }) {
            s.push_str(
                "\nYour answer is returned to the agent that delegated this task as its result. Answer with the result itself.\n",
            );
        }
        s.trim_end().to_string()
    }
}

pub fn short_id(id: &str) -> &str {
    &id[..id.len().min(12)]
}

pub fn clip(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{ArtifactRef, TaskState, Usage};
    use crate::spec::Budget;

    fn task(id: &str, owner: &str) -> TaskRecord {
        TaskRecord {
            id: id.into(),
            project: "p".into(),
            owner: owner.into(),
            objective: "Summarize the report".into(),
            context: "Focus on costs.".into(),
            origin: Origin::Agent {
                agent: "lead".into(),
                task: "t-1".into(),
            },
            parent: Some("t-1".into()),
            children: vec![],
            after: vec![],
            inputs: vec![ArtifactRef {
                id: "0123456789abcdef".into(),
                name: "report.md".into(),
                kind: "text".into(),
            }],
            state: TaskState::Queued,
            waiting_on: None,
            attempts: 0,
            max_attempts: 1,
            budget: Budget::default().steps(4),
            usage: Usage::default(),
            result: None,
            error: None,
            outputs: vec![],
            tool_calls: vec![],
            rules: vec![],
            rules_held_back: 0,
            brief_tokens: 0,
            model: String::new(),
            created_ms: 0,
            started_ms: None,
            finished_ms: None,
            run_ms: 0,
            pid: None,
        }
    }

    #[test]
    fn a_brief_holds_the_task_and_nothing_else() {
        let t = task("t-2", "worker");
        let mut dep = task("t-0", "other");
        dep.result = Some("x".repeat(5000));
        let b = Brief {
            task: &t,
            deps: vec![&dep],
            children: vec![],
            inbox: &[],
            resumed: false,
        }
        .render();
        assert!(b.starts_with("Task t-2 delegated by lead"));
        assert!(b.contains("== OBJECTIVE ==\nSummarize the report"));
        assert!(b.contains("== CONTEXT FROM lead ==\nFocus on costs."));
        assert!(b.contains("art:0123456789ab report.md (text)"));
        assert!(b.contains("result of t-0 (other)"));
        assert!(b.len() < 2000, "a dependency's result is clipped");
        assert!(b.contains("4 steps"));
    }
}
