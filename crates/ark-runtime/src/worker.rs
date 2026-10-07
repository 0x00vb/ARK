//! An agent whose loop runs elsewhere (ADR 0039, ADR 0043): another agent program does a task
//! with its own tools, and ARK keeps the task. A worker gets what [`Harness::ask`] gets (the
//! brief, the folder, the approvals, the cancel token) and reports as the harness does, with
//! [`HarnessEvent`]s: its model calls (usage), its own steps (tool calls), its answer. Whatever
//! it wants to change asks through the gate, so a question is a kept question like any other.
//!
//! The work model never names a worker's vendor or protocol: an app hands a [`Workers`]
//! catalog to the runtime, and an agent's model spec picks one (`agent:<name>`).
//!
//! [`Harness::ask`]: crate::harness::Harness::ask

use std::path::Path;
use std::sync::Arc;

use crate::approval::ApprovalGate;
use crate::cancel::CancelToken;
use crate::harness::HarnessEvent;

/// What an agent's model spec looks like when it names a worker: `agent:<name>` or
/// `agent:<name>:<model>`.
pub const SPEC: &str = "agent:";

/// The actions of a worker's requests: to change files (`{paths}`), to run a command
/// (`{command, folder}`), anything else (asked every time). A grant names the files or the
/// command; leave to change a file carries over a restart, leave to run a command does not.
pub const CHANGE: &str = "program.change";
pub const RUN: &str = "program.run";
pub const OTHER: &str = "program.other";

/// One task, as a worker gets it.
pub struct WorkerJob<'a> {
    pub brief: &'a str,
    /// The folder the task works in. A worker's changes are found here (`crate::folder`).
    pub folder: &'a Path,
    /// The model the worker should use, when the spec names one. Set per job, never taken
    /// from the worker's own settings.
    pub model: Option<&'a str>,
    /// Asks the person, on behalf of the task. `None`: changes are refused unless
    /// `allow_mutate`.
    pub approvals: Option<Arc<ApprovalGate>>,
    /// The agent may change files in the folder without asking. Commands still ask.
    pub allow_mutate: bool,
    pub cancel: CancelToken,
    /// What the worker kept for this task before ([`WorkerJob::keep`]) in an earlier run, when
    /// the task runs again after a restart.
    pub kept: Option<serde_json::Value>,
    /// Keep something for the next run of this task (the worker's own session, its process):
    /// saved with the task, given back as `kept`.
    pub keep: &'a (dyn Fn(serde_json::Value) + Send + Sync),
    /// The worker's events, as the harness reports its own.
    pub on_event: &'a (dyn Fn(HarnessEvent) + Send + Sync),
}

/// How a worker's run ended.
#[derive(Debug, Default)]
pub struct WorkerRun {
    /// Its last message: the task's answer.
    pub answer: String,
    /// What it reported the run cost, in millionths of a US dollar, when it reports cost.
    pub cost_micro_usd: Option<u64>,
    /// Stopped by the job's cancel token before it finished.
    pub cancelled: bool,
}

#[async_trait::async_trait]
pub trait Worker: Send + Sync {
    /// Its name in the catalog (`claude-code`).
    fn name(&self) -> &str;
    /// Do the task. `Err` is the reason it failed, in words.
    async fn run(&self, job: WorkerJob<'_>) -> Result<WorkerRun, String>;
    /// A run of this task in a process that is gone left this behind (`kept`): stop what is
    /// still running of it before the task runs again.
    fn stop_left(&self, _kept: &serde_json::Value) {}
}

/// The workers an app offers.
pub trait Workers: Send + Sync {
    /// The worker an `agent:<name>[:<model>]` spec names, if this catalog has it: `Err` when
    /// it is known but cannot run here (not installed), with what to do.
    fn worker(&self, name: &str) -> Option<Result<Arc<dyn Worker>, String>>;
}

/// `(name, model)` of a worker spec, `None` for any other spec.
pub fn parse_spec(spec: &str) -> Option<(&str, Option<&str>)> {
    let rest = spec.strip_prefix(SPEC)?;
    let (name, model) = match rest.split_once(':') {
        Some((n, m)) => (n, Some(m).filter(|m| !m.is_empty())),
        None => (rest, None),
    };
    (!name.is_empty()).then_some((name, model))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specs() {
        assert_eq!(parse_spec("agent:claude-code"), Some(("claude-code", None)));
        assert_eq!(
            parse_spec("agent:claude-code:sonnet"),
            Some(("claude-code", Some("sonnet")))
        );
        assert_eq!(parse_spec("claude:sonnet"), None);
        assert_eq!(parse_spec("agent:"), None);
    }
}
