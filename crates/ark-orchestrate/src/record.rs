//! The orchestrator's state, as stored: one [`AgentRecord`] per agent and one [`TaskRecord`]
//! per task. The scheduler works from these records, and the graph is read from them, so a
//! view never keeps state of its own.

use ark_core::RuleRef;
use ark_runtime::{ApprovalDecision, ApprovalRequest, ContextReport};
use serde::{Deserialize, Serialize};

use crate::spec::{AgentSpec, Budget, TaskSpec};

/// Unix time in milliseconds.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    /// No task running.
    Idle,
    /// Running a task.
    Running,
    /// Its task is waiting on tasks it delegated.
    Waiting,
    /// Takes no new work until resumed; a task it was running is suspended too.
    Suspended,
    /// Gone: takes no work, its model session is closed.
    Terminated,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    /// Waiting for the tasks it runs after.
    Pending,
    /// Ready, waiting for its agent or for provider capacity (`waiting_on` says which).
    Queued,
    Running,
    /// Its agent is waiting on tasks it delegated.
    Waiting,
    /// Stopped by a suspend; runs again on resume.
    Suspended,
    Done,
    Failed,
    Cancelled,
}

impl TaskState {
    pub fn is_final(self) -> bool {
        matches!(
            self,
            TaskState::Done | TaskState::Failed | TaskState::Cancelled
        )
    }
    pub fn as_str(self) -> &'static str {
        match self {
            TaskState::Pending => "pending",
            TaskState::Queued => "queued",
            TaskState::Running => "running",
            TaskState::Waiting => "waiting",
            TaskState::Suspended => "suspended",
            TaskState::Done => "done",
            TaskState::Failed => "failed",
            TaskState::Cancelled => "cancelled",
        }
    }
}

/// Who created a task.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "by", rename_all = "snake_case")]
pub enum Origin {
    /// The application (or a person through it).
    External,
    /// An agent, from inside one of its tasks.
    Agent { agent: String, task: String },
}

/// Token and call counts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cached_tokens: u64,
    pub model_calls: u32,
    pub tool_calls: u32,
    pub steps: u32,
    /// False once any call's usage was estimated.
    #[serde(default = "yes")]
    pub measured: bool,
    /// What the work cost as its agent reported it, in millionths of a US dollar (an agent
    /// program that reports cost; ARK's own agent's cost comes from its model calls).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub cost_micro_usd: u64,
    /// How many times the person was asked.
    #[serde(default, skip_serializing_if = "is_zero32")]
    pub asked: u32,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

fn is_zero32(n: &u32) -> bool {
    *n == 0
}

impl Usage {
    pub fn tokens(&self) -> u64 {
        self.prompt_tokens + self.completion_tokens
    }
    pub fn add(&mut self, o: &Usage) {
        self.prompt_tokens += o.prompt_tokens;
        self.completion_tokens += o.completion_tokens;
        self.cached_tokens += o.cached_tokens;
        self.model_calls += o.model_calls;
        self.tool_calls += o.tool_calls;
        self.steps += o.steps;
        self.measured &= o.measured;
        self.cost_micro_usd += o.cost_micro_usd;
        self.asked += o.asked;
    }
}

/// One tool call of a task, kept on the task for inspection.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolCallRecord {
    pub id: String,
    pub action: String,
    /// `None` while it runs.
    pub ok: Option<bool>,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub invocation: String,
    #[serde(default)]
    pub duration_ms: Option<i64>,
    /// Reused an earlier result instead of running.
    #[serde(default)]
    pub reused: bool,
}

/// The call an agent is making, or made last, in the task it runs now (ADR 0036). Kept by the
/// orchestrator from the run's events, so every client shows the same "doing now".
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Step {
    pub task: String,
    pub action: String,
    /// The input as the model gave it, long strings clipped.
    pub input: serde_json::Value,
    pub since_ms: u64,
}

/// A question a task waits for the person to answer. It is kept on the task, so any client
/// answers it and a restart does not lose it (ADR 0036).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Question {
    pub id: String,
    pub request: ApprovalRequest,
    pub asked_ms: u64,
    /// An answer given after the run that asked had stopped (a restart). The task runs again,
    /// and this answers the same call when it asks again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<ApprovalDecision>,
}

/// An answer the person gave in a task. When the process stops under the task, the run that
/// continues it takes the answer for the same call instead of asking again (`carry`); a pause, a
/// retry or a new run asks again.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Decision {
    /// A digest of the call: its action and input (the input can hold a file's content).
    pub key: String,
    pub decision: ApprovalDecision,
    /// The run (`TaskRecord::attempts`) it was given in.
    pub attempt: u32,
    /// The process stopped in that run: the next run continues it.
    #[serde(default)]
    pub carry: bool,
}

impl Decision {
    /// The digest of a call, as `key` holds it.
    pub fn key_of(action: &str, input: &serde_json::Value) -> String {
        let mut h = blake3::Hasher::new();
        h.update(action.as_bytes());
        h.update(b"\n");
        h.update(input.to_string().as_bytes());
        h.finalize().to_hex()[..32].to_string()
    }
}

impl Question {
    /// Still waiting for the person.
    pub fn open(&self) -> bool {
        self.answer.is_none()
    }
}

/// Tool calls kept per task; older ones stay in the event log.
pub const TOOL_CALLS_KEPT: usize = 50;

/// An artifact a task produced or was given.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRef {
    pub id: String,
    pub name: String,
    pub kind: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentRecord {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creation_request: Option<String>,
    pub name: String,
    /// The orchestrator's project.
    pub project: String,
    /// The agent's own project: its working state, evidence and conversation.
    pub scope: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creation_task: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creation_root: Option<String>,
    pub spec: AgentSpec,
    pub state: AgentState,
    /// The first of `running`, written for older readers. Read only to migrate a record.
    #[serde(default)]
    pub current_task: Option<String>,
    /// The tasks it runs now, at most one per conversation. The only record of what runs.
    #[serde(default)]
    pub running: Vec<String>,
    /// Its ARK conversation session (carry-over between its tasks).
    #[serde(default)]
    pub session: Option<String>,
    /// The model it runs on, as resolved (`claude:sonnet`).
    #[serde(default)]
    pub model: String,
    /// The account whose capacity it draws on.
    #[serde(default)]
    pub account: String,
    #[serde(default)]
    pub usage: Usage,
    #[serde(default)]
    pub tasks_done: u32,
    #[serde(default)]
    pub tasks_failed: u32,
    /// The last request it assembled, by source.
    #[serde(default)]
    pub last_context: Option<ContextReport>,
    /// Messages not yet delivered (it had no running task); they go in its next brief.
    #[serde(default)]
    pub inbox: Vec<Mail>,
    pub created_ms: u64,
    #[serde(default)]
    pub updated_ms: u64,
}

impl AgentRecord {
    /// It runs on an agent program (`agent:<name>`), which holds one conversation.
    pub fn on_program(&self) -> bool {
        self.spec
            .model
            .as_deref()
            .and_then(ark_runtime::worker::parse_spec)
            .is_some()
    }

    /// `task` runs now.
    pub(crate) fn start_running(&mut self, task: &str) {
        if !self.running.iter().any(|t| t == task) {
            self.running.push(task.to_string());
        }
        self.current_task = self.running.first().cloned();
    }

    /// `task` no longer runs.
    pub(crate) fn stop_running(&mut self, task: &str) {
        self.running.retain(|t| t != task);
        self.current_task = self.running.first().cloned();
    }
}

/// A message between agents.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mail {
    pub from: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub context: String,
    #[serde(default)]
    pub task: Option<String>,
    pub at_ms: u64,
    /// The conversation it is for ([`TaskRecord::thread`]); `None`: the default one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<String>,
}

/// A picture as it was when the task was admitted. The run reads the file again and fails when
/// it is another file (ADR 0121).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskImage {
    pub path: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskRecord {
    pub id: String,
    pub project: String,
    pub owner: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submission: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_step: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<String>,
    /// The agent's own conversation it runs in ([`crate::TaskSpec::thread`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<String>,
    pub objective: String,
    #[serde(default)]
    pub context: String,
    pub origin: Origin,
    /// The person typed it in a chat ([`crate::TaskSpec::person`]).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub person: bool,
    /// A group chat's driver submitted it ([`crate::TaskSpec::group_turn`]).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub group_turn: bool,
    /// The pictures the person attached, as admitted ([`crate::TaskSpec::images`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<TaskImage>,
    /// The task that delegated this one.
    #[serde(default)]
    pub parent: Option<String>,
    /// Tasks this one delegated, in order.
    #[serde(default)]
    pub children: Vec<String>,
    #[serde(default)]
    pub after: Vec<String>,
    #[serde(default)]
    pub inputs: Vec<ArtifactRef>,
    pub state: TaskState,
    /// Why it is not running: the tasks it waits for, the agent being busy, provider
    /// capacity, a suspend.
    #[serde(default)]
    pub waiting_on: Option<String>,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default = "one")]
    pub max_attempts: u32,
    #[serde(default)]
    pub budget: Budget,
    #[serde(default)]
    pub usage: Usage,
    #[serde(default)]
    pub result: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub outputs: Vec<ArtifactRef>,
    #[serde(default)]
    pub tool_calls: Vec<ToolCallRecord>,
    /// Rules given to the agent for this task.
    #[serde(default)]
    pub rules: Vec<RuleRef>,
    #[serde(default)]
    pub rules_held_back: usize,
    /// Estimated tokens of the brief the agent received (its whole view of the task).
    #[serde(default)]
    pub brief_tokens: u32,
    /// The model the task ran on.
    #[serde(default)]
    pub model: String,
    pub created_ms: u64,
    #[serde(default)]
    pub started_ms: Option<u64>,
    #[serde(default)]
    pub finished_ms: Option<u64>,
    /// Time spent running (not queued or waiting), in milliseconds.
    #[serde(default)]
    pub run_ms: u64,
    /// The process running it, while it runs.
    #[serde(default)]
    pub pid: Option<u32>,
    /// When that process started, to tell it from a later one with the same pid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid_start: Option<u64>,
    /// The schedule that started it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<String>,
    /// For a schedule's run: the time it is for (Unix ms). A run is never started twice for one
    /// time, also across a crash.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot: Option<u64>,
    /// The wait before another try after a failure that may pass, doubling at each one
    /// ([`crate::TaskSpec::retry_after`]). `None`: half a second, doubling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_delay_ms: Option<u64>,
    /// When it tries again after a failure (Unix ms), while it waits; a restart waits too.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_at_ms: Option<u64>,
    /// The board item it works on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
    /// The chat its result is posted into when it ends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report_to: Option<String>,
    /// Its result is posted as the agent's own message ([`crate::TaskSpec::reply`]).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reply: bool,
    /// The work that handed it off ([`crate::TaskSpec::work`]); a turn that continues that
    /// work and takes it up writes its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work: Option<String>,
    /// Its result reached the chat it reports to: as the answer to that chat's own wait for it,
    /// or as a post. Only a finished task's counts; a restart posts a result that did not
    /// (ADR 0038). A record saved before this field existed was reported when it finished.
    #[serde(default = "yes")]
    pub reported: bool,
    /// The question it waits for the person to answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question: Option<Question>,
    /// The answers given in this task, for its later runs ([`Decision`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decisions: Vec<Decision>,
}

fn one() -> u32 {
    1
}

fn yes() -> bool {
    true
}

impl TaskRecord {
    /// A turn of a chat with the agent: the person's own or a group chat's member turn. Not
    /// delegated or started by an agent, scheduled, an automation, Work, a board item's, or
    /// work already handed off by `work.later` (ADR 0118).
    pub(crate) fn is_chat_turn(&self) -> bool {
        self.origin == Origin::External
            && (self.person || self.group_turn)
            && self.run.is_none()
            && self.schedule.is_none()
            && self.conversation.is_none()
            && self.submission.is_none()
            && self.item.is_none()
            && !self.reply
    }

    pub(crate) fn queued(
        id: String,
        project: String,
        owner: String,
        spec: &TaskSpec,
        origin: Origin,
        budget: Budget,
        inputs: Vec<ArtifactRef>,
    ) -> Self {
        Self {
            id,
            project,
            owner,
            handoff_key: None,
            submission: None,
            run: None,
            execution_scope: None,
            execution_step: None,
            execution_owner: None,
            conversation: None,
            thread: spec.thread.clone(),
            objective: spec.objective.clone(),
            context: spec.context.clone(),
            origin,
            person: spec.person,
            group_turn: spec.group_turn,
            images: Vec::new(),
            parent: None,
            children: Vec::new(),
            after: spec.after.clone(),
            inputs,
            state: if spec.after.is_empty() {
                TaskState::Queued
            } else {
                TaskState::Pending
            },
            waiting_on: None,
            attempts: 0,
            max_attempts: spec.max_attempts.unwrap_or(1).max(1),
            budget,
            usage: Usage::default(),
            result: None,
            error: None,
            outputs: Vec::new(),
            tool_calls: Vec::new(),
            rules: Vec::new(),
            rules_held_back: 0,
            brief_tokens: 0,
            model: String::new(),
            created_ms: now_ms(),
            started_ms: None,
            finished_ms: None,
            run_ms: 0,
            pid: None,
            pid_start: None,
            schedule: None,
            slot: None,
            retry_delay_ms: spec.retry_delay_ms,
            retry_at_ms: None,
            item: spec.item.clone(),
            report_to: spec.report_to.clone(),
            reply: spec.reply,
            work: spec.work.clone(),
            reported: false,
            question: None,
            decisions: Vec::new(),
        }
    }

    /// Milliseconds from start to finish (or to now, while it runs).
    pub fn elapsed_ms(&self) -> u64 {
        match (self.started_ms, self.finished_ms) {
            (Some(s), Some(f)) => f.saturating_sub(s),
            (Some(s), None) => now_ms().saturating_sub(s),
            _ => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_task_stored_before_pictures_loads_and_writes_without_them() {
        let spec = TaskSpec::new("x");
        let new = TaskRecord::queued(
            "t-1".into(),
            "p".into(),
            "w".into(),
            &spec,
            Origin::External,
            Budget::default(),
            vec![],
        );
        let mut stored = serde_json::to_value(&new).unwrap();
        assert!(stored.get("images").is_none());
        stored.as_object_mut().unwrap().remove("images");
        let old: TaskRecord = serde_json::from_value(stored).unwrap();
        assert!(old.images.is_empty());
        let mut with = new.clone();
        with.images = vec![TaskImage {
            path: "a.png".into(),
            sha256: "ab".into(),
            size: 2,
        }];
        let back: TaskRecord =
            serde_json::from_value(serde_json::to_value(&with).unwrap()).unwrap();
        assert_eq!(back.images, with.images);
    }

    #[test]
    fn a_graph_node_stored_before_pictures_loads() {
        let node: crate::graph::TaskNode = serde_json::from_value(serde_json::json!({
            "id": "task:t-1", "task": "t-1", "owner": "w", "objective": "x",
            "origin": serde_json::to_value(Origin::External).unwrap(), "parent": null, "children": [], "after": [],
            "state": "done", "waiting_on": null, "waiting_for": [], "attempts": 1,
            "max_attempts": 1, "elapsed_ms": 0, "run_ms": 0,
            "usage": serde_json::to_value(Usage::default()).unwrap(), "budget": {}, "tool_calls": [], "inputs": [], "outputs": [],
            "rules": [], "rules_held_back": 0, "brief_tokens": 0, "model": "",
            "result": null, "error": null, "created_ms": 0, "started_ms": null,
            "finished_ms": null
        }))
        .unwrap();
        assert!(node.images.is_empty());
    }
}
