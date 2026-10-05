//! # ark-orchestrate
//!
//! Several agents working together, as compositions of a few primitives. ARK provides the
//! mechanism; an application decides what its agents are (a reviewer and two writers, a
//! research team, a hierarchy of departments).
//!
//! | Primitive | What it is |
//! |---|---|
//! | [`AgentSpec`] | an agent's identity, instructions, tools, permissions and model |
//! | [`TaskSpec`] / [`TaskRecord`] | an objective, its state, owner, dependencies, artifacts and [`Budget`] |
//! | delegation | `agents.delegate`: an agent creates a task for an agent it may delegate to |
//! | the person's chat | [`Orchestrator::chat_tools`]: the conversation creates agents and hands them tasks; [`Orchestrator::board_tools`]: it plans on the board |
//! | messages | `agents.message`: a short note into another agent's running task or inbox |
//! | artifacts | passed explicitly with a task and handed back with its result |
//! | [`Orchestrator`] | lifecycle (spawn, run, suspend, resume, cancel, terminate) and a capacity-aware scheduler |
//! | [`ScheduleRecord`] / [`When`] | a task an agent runs on its own at set times (`every weekday at 09:00`) |
//! | [`WorkItem`] | the project's board: work the person and agents share; it follows the tasks run for it ([`board`]) |
//! | [`Graph`] / [`OrchEvent`] | the live state and the full trace, for any front-end |
//!
//! Each agent works in its own child project, so its working state, evidence and
//! conversation are its own, and on its own managed model session (one vendor session per
//! agent for a subscription, see [`ark_model::session`]). A delegated agent receives a brief:
//! the objective, the context its delegator chose to give, the artifacts named, and the
//! results of the tasks it runs after; never another agent's conversation.
//!
//! ```ignore
//! let orch = Orchestrator::builder(&ark, &project).build()?;
//! orch.spawn(AgentSpec::new("lead").delegates_to(["reader-a", "reader-b"]).model("claude:sonnet")).await?;
//! orch.spawn(AgentSpec::new("reader-a").role("reads one document").model("codex")).await?;
//! orch.spawn(AgentSpec::new("reader-b").role("reads one document").model("claude:haiku")).await?;
//! let done = orch.run("lead", TaskSpec::new("compare the two reports")).await?;
//! println!("{}", done.result.unwrap_or_default());
//! println!("{}", orch.graph().to_mermaid());
//! ```

pub mod board;
mod brief;
mod event;
mod graph;
pub mod models;
mod orchestrator;
mod record;
pub mod schedule;
mod spec;
mod tools;

pub use board::{WorkChange, WorkItem, WorkState, WorkStep};
pub use brief::Brief;
pub use event::{EventKind, OrchEvent};
pub use graph::{AgentNode, ContextUse, Edge, EdgeKind, Graph, MessageEdge, TaskNode};
pub use models::{AgentModels, ModelResolver, Models, ModelsBuilder, StoreLedger};
pub use orchestrator::{
    NamedProfile, Orchestrator, OrchestratorBuilder, INTERRUPTED, WAITING_PERSON,
};
pub use record::{
    AgentRecord, AgentState, ArtifactRef, Mail, Origin, Question, Step, TaskRecord, TaskState,
    ToolCallRecord, Usage,
};
pub use schedule::{projects_with_work, scheduled_projects, ScheduleRecord, When};
pub use spec::{AgentSpec, Budget, TaskSpec};
pub use tools::{
    BOARD_ADD, BOARD_LIST, BOARD_UPDATE, CHAT, CREATE, DELEGATE, MESSAGE, SCHEDULE, UNSCHEDULE,
    WAIT,
};

#[derive(Debug, thiserror::Error)]
pub enum OrchError {
    #[error("no agent named `{0}`")]
    UnknownAgent(String),
    #[error("no task `{0}`")]
    UnknownTask(String),
    #[error("agent `{0}` is terminated")]
    Terminated(String),
    #[error("{0}")]
    NotAllowed(String),
    #[error(transparent)]
    Runtime(#[from] ark_runtime::AgentError),
    #[error(transparent)]
    Store(#[from] ark_store::StoreError),
    #[error(transparent)]
    Provider(#[from] ark_model::ProviderError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
