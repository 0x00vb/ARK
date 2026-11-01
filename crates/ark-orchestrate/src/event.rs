//! The orchestration trace. Every change to an agent or a task, every delegation and message,
//! and every runtime event an agent emits while working, as one ordered stream per project.
//! Events are stored (streamed text excepted) and broadcast live; a front-end loads a
//! [`crate::Graph`] and then follows the stream from the graph's `seq`.

use ark_core::RuleRef;
use ark_model::SessionEvent;
use ark_runtime::HarnessEvent;
use serde::{Deserialize, Serialize};

use crate::record::{AgentState, ArtifactRef, Origin, TaskState, Usage};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OrchEvent {
    /// Position in the project's stream (0 for an event that was not stored).
    pub seq: u64,
    pub at_ms: u64,
    pub project: String,
    #[serde(flatten)]
    pub kind: EventKind,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventKind {
    Automation {
        automation: String,
        run: Option<String>,
    },
    AgentSpawned {
        agent: String,
        parent: Option<String>,
        model: String,
    },
    AgentState {
        agent: String,
        state: AgentState,
    },
    TaskCreated {
        task: String,
        owner: String,
        origin: Origin,
        parent: Option<String>,
        objective: String,
        after: Vec<String>,
    },
    /// The task cannot start yet; `reason` says what it waits for.
    TaskQueued {
        task: String,
        reason: String,
    },
    TaskStarted {
        task: String,
        agent: String,
        attempt: u32,
        model: String,
        /// Estimated tokens of the brief the agent received.
        brief_tokens: u32,
    },
    /// An agent handed a task to another.
    Delegated {
        from_agent: String,
        from_task: String,
        to_agent: String,
        task: String,
    },
    /// A task waits on tasks it delegated (its capacity is released meanwhile).
    TaskWaiting {
        task: String,
        on: Vec<String>,
    },
    TaskResumed {
        task: String,
    },
    /// A task's agent asks the person before a state-changing call; the task waits for the
    /// answer (its `question`).
    Asked {
        task: String,
        agent: String,
        question: String,
        preview: String,
    },
    /// The person answered a task's question.
    Answered {
        task: String,
        question: String,
        decision: ark_runtime::ApprovalDecision,
    },
    Message {
        from: String,
        to: String,
        task: Option<String>,
        text: String,
        /// Delivered into a running task (true) or kept for the next one.
        delivered: bool,
    },
    RulesApplied {
        task: String,
        applied: Vec<RuleRef>,
        held_back: usize,
    },
    /// Something the agent's runtime did while working on `task`.
    Agent {
        task: String,
        agent: String,
        event: HarnessEvent,
    },
    /// Something that happened to the agent's vendor session.
    Session {
        agent: String,
        event: SessionEvent,
    },
    ArtifactProduced {
        task: String,
        agent: String,
        artifact: ArtifactRef,
    },
    /// A failed attempt will run again after `delay_ms`.
    TaskRetry {
        task: String,
        attempt: u32,
        error: String,
        delay_ms: u64,
    },
    BudgetExceeded {
        task: String,
        limit: String,
    },
    TaskFinished {
        task: String,
        state: TaskState,
        result: Option<String>,
        error: Option<String>,
        usage: Usage,
        run_ms: u64,
    },
    /// `agent` runs `objective` on a schedule from now on (`says`: when, in words).
    Scheduled {
        schedule: String,
        agent: String,
        objective: String,
        says: String,
    },
    /// A board item was added or changed (`by`: an agent, `user`, or `chat`).
    Work {
        item: crate::board::WorkItem,
        by: String,
    },
    /// A board item was removed.
    WorkRemoved {
        item: String,
        title: String,
    },
    /// A board was made or its goal changed (`by`: an agent, `user`, or `chat`).
    Board {
        board: crate::board::Board,
        by: String,
    },
    /// A board was removed, with its items.
    BoardRemoved {
        board: String,
        goal: String,
    },
    /// A task's result was posted into the chat it reports to (`session`).
    Posted {
        session: String,
        task: String,
        post: ark_runtime::Post,
    },
    /// A schedule was paused, resumed or changed (`says`: when, in words, after the change).
    ScheduleChanged {
        schedule: String,
        agent: String,
        says: String,
        paused: bool,
    },
    /// A schedule stopped (asked to, or its agent ended).
    Unscheduled {
        schedule: String,
        #[serde(default)]
        agent: String,
        #[serde(default)]
        says: String,
    },
}

impl EventKind {
    /// The task this event belongs to, when it belongs to one.
    pub fn task(&self) -> Option<&str> {
        match self {
            EventKind::TaskCreated { task, .. }
            | EventKind::TaskQueued { task, .. }
            | EventKind::TaskStarted { task, .. }
            | EventKind::Delegated { task, .. }
            | EventKind::TaskWaiting { task, .. }
            | EventKind::TaskResumed { task }
            | EventKind::Asked { task, .. }
            | EventKind::Answered { task, .. }
            | EventKind::RulesApplied { task, .. }
            | EventKind::Agent { task, .. }
            | EventKind::ArtifactProduced { task, .. }
            | EventKind::TaskRetry { task, .. }
            | EventKind::BudgetExceeded { task, .. }
            | EventKind::TaskFinished { task, .. }
            | EventKind::Posted { task, .. } => Some(task),
            EventKind::Message { task, .. } => task.as_deref(),
            _ => None,
        }
    }

    /// Whether a task's trace includes this event: its own events, and a delegation it made.
    pub fn concerns_task(&self, id: &str) -> bool {
        self.task() == Some(id)
            || matches!(self, EventKind::Delegated { from_task, .. } if from_task == id)
    }

    /// The agent this event is about.
    pub fn agent(&self) -> Option<&str> {
        match self {
            EventKind::AgentSpawned { agent, .. }
            | EventKind::AgentState { agent, .. }
            | EventKind::Agent { agent, .. }
            | EventKind::Session { agent, .. }
            | EventKind::ArtifactProduced { agent, .. }
            | EventKind::Scheduled { agent, .. }
            | EventKind::ScheduleChanged { agent, .. }
            | EventKind::Asked { agent, .. }
            | EventKind::TaskStarted { agent, .. } => Some(agent),
            EventKind::TaskCreated { owner, .. } => Some(owner),
            EventKind::Delegated { from_agent, .. } => Some(from_agent),
            EventKind::Message { from, .. } => Some(from),
            _ => None,
        }
    }

    /// Streamed text is live-only: it is broadcast, never stored.
    pub(crate) fn stored(&self) -> bool {
        !matches!(
            self,
            EventKind::Agent {
                event: HarnessEvent::TextDelta { .. },
                ..
            }
        )
    }
}
