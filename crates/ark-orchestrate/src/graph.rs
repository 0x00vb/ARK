//! The orchestration graph: a snapshot of agents, tasks and how they connect, read from the
//! orchestrator's own records and its model sessions. A terminal, a window, a web page or a
//! phone renders the same [`Graph`]; to follow it live, load one and apply the events after
//! [`Graph::seq`] (see [`crate::Orchestrator::subscribe`] and
//! [`crate::Orchestrator::events`]).
//!
//! Node ids are prefixed by type: `agent:<name>`, `task:<id>`, `art:<id>`.

use ark_core::RuleRef;
use ark_model::{CapacitySnapshot, SessionSnapshot};
use serde::{Deserialize, Serialize};

use crate::brief::clip;
use crate::orchestrator::Inner;
use crate::record::{
    now_ms, AgentState, ArtifactRef, Origin, Question, Step, TaskState, ToolCallRecord, Usage,
};
use crate::spec::Budget;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Graph {
    pub project: String,
    /// The last stored event this snapshot includes.
    pub seq: u64,
    pub at_ms: u64,
    pub agents: Vec<AgentNode>,
    pub tasks: Vec<TaskNode>,
    pub edges: Vec<Edge>,
    /// Each account's capacity right now.
    pub accounts: Vec<CapacitySnapshot>,
    /// All tasks together.
    pub usage: Usage,
    /// Tasks agents run on their own at set times, soonest first.
    #[serde(default)]
    pub schedules: Vec<crate::ScheduleRecord>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentNode {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub look: String,
    pub role: String,
    pub parent: Option<String>,
    pub state: AgentState,
    /// The first of `running`, for older readers.
    pub current_task: Option<String>,
    /// The tasks it runs now, one per conversation at most.
    #[serde(default)]
    pub running: Vec<String>,
    pub model: String,
    pub account: String,
    /// Its managed vendor session, when it runs on a subscription.
    pub session: Option<SessionSnapshot>,
    /// Its last request against its context budget.
    pub context: Option<ContextUse>,
    pub usage: Usage,
    pub tasks_done: u32,
    pub tasks_failed: u32,
    pub delegates_to: Vec<String>,
    pub tools: Vec<String>,
    /// The named profile it starts from; `None`: the base profile.
    #[serde(default)]
    pub profile: Option<String>,
    /// Messages waiting for its next task.
    pub inbox: usize,
    /// The call it is making, or made last, in the task it runs now. What its task waits on
    /// is the task's `waiting_on` and `question`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<Step>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct ContextUse {
    pub tokens: u32,
    pub budget: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskNode {
    pub id: String,
    pub task: String,
    pub owner: String,
    pub objective: String,
    /// The agent's own conversation it runs in ([`crate::TaskSpec::thread`]).
    #[serde(default)]
    pub thread: Option<String>,
    pub origin: Origin,
    /// The schedule that started it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<String>,
    pub parent: Option<String>,
    pub children: Vec<String>,
    pub after: Vec<String>,
    pub state: TaskState,
    pub waiting_on: Option<String>,
    /// Tasks it is waiting for right now.
    pub waiting_for: Vec<String>,
    pub attempts: u32,
    pub max_attempts: u32,
    pub elapsed_ms: u64,
    pub run_ms: u64,
    pub usage: Usage,
    pub budget: Budget,
    pub tool_calls: Vec<ToolCallRecord>,
    pub inputs: Vec<ArtifactRef>,
    pub outputs: Vec<ArtifactRef>,
    pub rules: Vec<RuleRef>,
    pub rules_held_back: usize,
    pub brief_tokens: u32,
    pub model: String,
    pub result: Option<String>,
    pub error: Option<String>,
    pub created_ms: u64,
    pub started_ms: Option<u64>,
    pub finished_ms: Option<u64>,
    /// The question it waits for the person to answer (ADR 0036).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question: Option<Question>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    /// agent → task it owns.
    Owns,
    /// task → task it delegated.
    Delegated,
    /// task → task it runs after.
    DependsOn,
    /// agent → agent it belongs to (topology).
    Parent,
    /// agent → agent it may delegate to.
    MayDelegate,
    /// agent → agent it sent a message.
    Message,
    /// task → artifact it produced.
    Produced,
    /// artifact → task it was given to.
    Input,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edge {
    pub kind: EdgeKind,
    pub from: String,
    pub to: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// A message between agents, as kept for the graph.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MessageEdge {
    pub from: String,
    pub to: String,
    pub task: Option<String>,
    pub at_ms: u64,
}

/// Characters of a task's result in the graph. The task record has all of it.
const RESULT_CHARS: usize = 600;

fn agent_id(name: &str) -> String {
    format!("agent:{name}")
}

fn task_id(id: &str) -> String {
    format!("task:{id}")
}

pub(crate) fn build(inner: &Inner) -> Graph {
    let sessions = inner.models.sessions();
    let accounts = inner.models.capacity();
    let seq = inner
        .engine
        .store
        .last_event(&format!("orch:{}", inner.project.id))
        .unwrap_or(0);
    let st = inner.state.lock().unwrap();
    let lane = |name: &str| format!("{}/{name}", inner.project.id);

    let mut agents: Vec<AgentNode> = st
        .agents
        .values()
        .map(|a| {
            let r = &a.record;
            AgentNode {
                id: agent_id(&r.name),
                name: r.name.clone(),
                title: r.spec.title.clone(),
                description: r.spec.description.clone(),
                look: r.spec.look.clone(),
                role: r.spec.role.clone(),
                parent: r.spec.parent.clone(),
                state: r.state,
                current_task: r.current_task.clone(),
                running: r.running.clone(),
                model: r.model.clone(),
                account: r.account.clone(),
                session: sessions
                    .iter()
                    .find(|s| s.lane == lane(&r.name))
                    .cloned()
                    .or_else(|| inner.models.stored_session(&lane(&r.name))),
                context: r.last_context.as_ref().map(|c| ContextUse {
                    tokens: c.total,
                    budget: c.budget,
                }),
                usage: r.usage,
                tasks_done: r.tasks_done,
                tasks_failed: r.tasks_failed,
                delegates_to: r.spec.delegates_to.clone(),
                tools: r.spec.tools.clone(),
                profile: r.spec.profile_name.clone(),
                inbox: r.inbox.len(),
                step: a.step.clone().filter(|s| r.running.contains(&s.task)),
            }
        })
        .collect();
    agents.sort_by(|a, b| a.name.cmp(&b.name));

    let mut tasks: Vec<TaskNode> = st
        .tasks
        .values()
        .map(|t| {
            let r = &t.record;
            TaskNode {
                id: task_id(&r.id),
                task: r.id.clone(),
                owner: r.owner.clone(),
                objective: r.objective.clone(),
                thread: r.thread.clone(),
                origin: r.origin.clone(),
                schedule: r.schedule.clone(),
                parent: r.parent.clone(),
                children: r.children.clone(),
                after: r.after.clone(),
                state: r.state,
                waiting_on: r.waiting_on.clone(),
                // Tasks only; waiting for the person shows as `question`.
                waiting_for: t
                    .waiting_for
                    .iter()
                    .filter(|w| !w.starts_with('@'))
                    .cloned()
                    .collect(),
                attempts: r.attempts,
                max_attempts: r.max_attempts,
                elapsed_ms: r.elapsed_ms(),
                run_ms: r.run_ms,
                usage: r.usage,
                budget: r.budget.clone(),
                tool_calls: r.tool_calls.clone(),
                inputs: r.inputs.clone(),
                outputs: r.outputs.clone(),
                rules: r.rules.clone(),
                rules_held_back: r.rules_held_back,
                brief_tokens: r.brief_tokens,
                model: r.model.clone(),
                result: r.result.as_deref().map(|x| clip(x, RESULT_CHARS)),
                error: r.error.clone(),
                created_ms: r.created_ms,
                started_ms: r.started_ms,
                finished_ms: r.finished_ms,
                question: r.question.clone().filter(|q| q.open()),
            }
        })
        .collect();
    tasks.sort_by_key(|t| (t.created_ms, t.task.clone()));

    let mut edges = Vec::new();
    for a in &agents {
        if let Some(p) = &a.parent {
            edges.push(Edge {
                kind: EdgeKind::Parent,
                from: agent_id(&a.name),
                to: agent_id(p),
                label: None,
            });
        }
        for d in &a.delegates_to {
            let targets: Vec<&str> = if d == "*" {
                agents
                    .iter()
                    .map(|x| x.name.as_str())
                    .filter(|n| *n != a.name)
                    .collect()
            } else {
                vec![d.as_str()]
            };
            for t in targets {
                edges.push(Edge {
                    kind: EdgeKind::MayDelegate,
                    from: agent_id(&a.name),
                    to: agent_id(t),
                    label: None,
                });
            }
        }
    }
    let mut usage = Usage::default();
    for t in &tasks {
        usage.add(&t.usage);
        edges.push(Edge {
            kind: EdgeKind::Owns,
            from: agent_id(&t.owner),
            to: t.id.clone(),
            label: None,
        });
        if let Some(p) = &t.parent {
            edges.push(Edge {
                kind: EdgeKind::Delegated,
                from: task_id(p),
                to: t.id.clone(),
                label: Some(t.owner.clone()),
            });
        }
        for d in &t.after {
            edges.push(Edge {
                kind: EdgeKind::DependsOn,
                from: t.id.clone(),
                to: task_id(d),
                label: None,
            });
        }
        for o in &t.outputs {
            edges.push(Edge {
                kind: EdgeKind::Produced,
                from: t.id.clone(),
                to: format!("art:{}", o.id),
                label: Some(o.name.clone()),
            });
        }
        for i in &t.inputs {
            edges.push(Edge {
                kind: EdgeKind::Input,
                from: format!("art:{}", i.id),
                to: t.id.clone(),
                label: Some(i.name.clone()),
            });
        }
    }
    for m in &st.messages {
        edges.push(Edge {
            kind: EdgeKind::Message,
            from: agent_id(&m.from),
            to: agent_id(&m.to),
            label: m.task.clone(),
        });
    }
    let mut schedules: Vec<crate::ScheduleRecord> = st.schedules.values().cloned().collect();
    schedules.sort_by_key(|s| (s.next_ms, s.created_ms));
    Graph {
        project: inner.project.id.to_string(),
        seq,
        at_ms: now_ms(),
        agents,
        tasks,
        edges,
        accounts,
        usage,
        schedules,
    }
}

impl Graph {
    pub fn agent(&self, name: &str) -> Option<&AgentNode> {
        self.agents.iter().find(|a| a.name == name)
    }

    pub fn task(&self, id: &str) -> Option<&TaskNode> {
        self.tasks.iter().find(|t| t.task == id)
    }

    /// Tasks with no parent task: what was asked from outside.
    pub fn roots(&self) -> Vec<&TaskNode> {
        self.tasks.iter().filter(|t| t.parent.is_none()).collect()
    }

    pub fn children_of(&self, id: &str) -> Vec<&TaskNode> {
        self.tasks
            .iter()
            .filter(|t| t.parent.as_deref() == Some(id))
            .collect()
    }

    /// The graph as a Mermaid flowchart (an orchestration diagram): one box per agent with
    /// its tasks inside, delegation and dependency edges between tasks. It passes
    /// [`ark_tools::diagram::validate`] for [`ark_tools::diagram::DiagramKind::Orchestration`].
    pub fn to_mermaid(&self) -> String {
        let mut s = String::from("flowchart LR\n");
        for a in &self.agents {
            s.push_str(&format!(
                "  subgraph {} [\"{} · {} · {}\"]\n",
                mermaid_id("agent", &a.name),
                label(&a.name),
                label(&a.model),
                state_word(a.state)
            ));
            for t in self.tasks.iter().filter(|t| t.owner == a.name) {
                s.push_str(&format!(
                    "    {}[\"{} · {}<br/>{}\"]\n",
                    mermaid_id("task", &t.task),
                    t.task,
                    t.state.as_str(),
                    label(&clip(&t.objective, 48))
                ));
            }
            s.push_str("  end\n");
        }
        for e in &self.edges {
            let (Some(from), Some(to)) = (task_ref(&e.from), task_ref(&e.to)) else {
                continue;
            };
            match e.kind {
                EdgeKind::Delegated => s.push_str(&format!(
                    "  {} -->|delegated| {}\n",
                    mermaid_id("task", from),
                    mermaid_id("task", to)
                )),
                EdgeKind::DependsOn => s.push_str(&format!(
                    "  {} -.->|after| {}\n",
                    mermaid_id("task", to),
                    mermaid_id("task", from)
                )),
                _ => {}
            }
        }
        for (state, style) in [
            (TaskState::Done, "fill:#e6f4ea,stroke:#2e7d32"),
            (TaskState::Failed, "fill:#fdecea,stroke:#c62828"),
            (TaskState::Running, "fill:#e8f0fe,stroke:#1a73e8"),
            (TaskState::Waiting, "fill:#fff8e1,stroke:#f9a825"),
        ] {
            let ids: Vec<String> = self
                .tasks
                .iter()
                .filter(|t| t.state == state)
                .map(|t| mermaid_id("task", &t.task))
                .collect();
            if !ids.is_empty() {
                s.push_str(&format!("  classDef {} {style}\n", state.as_str()));
                s.push_str(&format!("  class {} {}\n", ids.join(","), state.as_str()));
            }
        }
        s
    }
}

fn task_ref(node: &str) -> Option<&str> {
    node.strip_prefix("task:")
}

fn mermaid_id(prefix: &str, raw: &str) -> String {
    let body: String = raw
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    format!("{prefix}_{body}")
}

fn label(s: &str) -> String {
    s.replace('"', "'").replace(['\n', '\r'], " ")
}

fn state_word(s: AgentState) -> &'static str {
    match s {
        AgentState::Idle => "idle",
        AgentState::Running => "running",
        AgentState::Waiting => "waiting",
        AgentState::Suspended => "suspended",
        AgentState::Terminated => "terminated",
    }
}
