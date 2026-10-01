//! Invocations: the recorded execution of an action. This is the spine of the whole
//! system — reproducibility record, agent memory, and notebook source, all at once.
//! Nothing calls an external tool without producing one.

use crate::action::RiskTier;
use crate::ids::{ArtifactId, InvocationId};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvocationStatus {
    /// Created, waiting on human approval (a `Mutate` action).
    PendingApproval,
    Running,
    Ok,
    Failed,
    /// The human declined a pending action.
    Denied,
}

/// One recorded call to an action.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Invocation {
    pub id: InvocationId,
    /// Namespaced action name, e.g. `fs.read`.
    pub action: String,
    pub risk: RiskTier,
    /// The exact input the action was called with.
    pub input: serde_json::Value,
    /// The concrete external command line, when the adapter shells out. Recorded verbatim
    /// so a human can reproduce it by hand. `None` for in-process actions.
    pub command: Option<Vec<String>>,
    pub status: InvocationStatus,
    /// Structured result the action returned (the summary the agent reasons over).
    #[serde(default)]
    pub output: serde_json::Value,
    /// Artifacts this invocation produced (stdout captures, generated files…).
    #[serde(default)]
    pub produced: Vec<ArtifactId>,
    /// Artifacts this invocation consumed as input.
    #[serde(default)]
    pub consumed: Vec<ArtifactId>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub finished_at: Option<OffsetDateTime>,
    /// The process that ran the call. A call still `Running` whose process is gone was
    /// interrupted (see the runtime's recovery on open).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
}

impl Invocation {
    pub fn new(action: impl Into<String>, risk: RiskTier, input: serde_json::Value) -> Self {
        let status = if risk.auto_runs() {
            InvocationStatus::Running
        } else {
            InvocationStatus::PendingApproval
        };
        Invocation {
            id: InvocationId::new(),
            action: action.into(),
            risk,
            input,
            command: None,
            status,
            output: serde_json::Value::Null,
            produced: Vec::new(),
            consumed: Vec::new(),
            error: None,
            created_at: OffsetDateTime::now_utc(),
            finished_at: None,
            pid: None,
        }
    }

    pub fn mark_ok(&mut self, output: serde_json::Value) {
        self.output = output;
        self.status = InvocationStatus::Ok;
        self.finished_at = Some(OffsetDateTime::now_utc());
    }

    pub fn mark_failed(&mut self, error: impl Into<String>) {
        self.error = Some(error.into());
        self.status = InvocationStatus::Failed;
        self.finished_at = Some(OffsetDateTime::now_utc());
    }

    pub fn duration_ms(&self) -> Option<i64> {
        self.finished_at
            .map(|f| (f - self.created_at).whole_milliseconds() as i64)
    }
}
