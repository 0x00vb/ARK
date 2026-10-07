//! The orchestrator's state, as stored: one [`AgentRecord`] per agent and one [`TaskRecord`]
//! per task. The scheduler works from these records, and the graph is read from them, so a
//! view never keeps state of its own.

use ark_core::RuleRef;
use ark_runtime::{ApprovalDecision, ApprovalRequest, ContextReport};
use serde::{Deserialize, Serialize};

use crate::spec::{AgentSpec, Budget};

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
    pub name: String,
    /// The orchestrator's project.
    pub project: String,
    /// The agent's own project: its working state, evidence and conversation.
    pub scope: String,
    pub spec: AgentSpec,
    pub state: AgentState,
    #[serde(default)]
    pub current_task: Option<String>,
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

/// A message between agents.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mail {
    pub from: String,
    pub text: String,
    #[serde(default)]
    pub task: Option<String>,
    pub at_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskRecord {
    pub id: String,
    pub project: String,
    pub owner: String,
    pub objective: String,
    #[serde(default)]
    pub context: String,
    pub origin: Origin,
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
    /// The schedule that started it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<String>,
    /// The board item it works on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
    /// The chat its result is posted into when it ends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report_to: Option<String>,
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
    /// Milliseconds from start to finish (or to now, while it runs).
    pub fn elapsed_ms(&self) -> u64 {
        match (self.started_ms, self.finished_ms) {
            (Some(s), Some(f)) => f.saturating_sub(s),
            (Some(s), None) => now_ms().saturating_sub(s),
            _ => 0,
        }
    }
}
