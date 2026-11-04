//! Reviewed corrections scoped to one project. Memory is data, never approval authority.
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::{InvocationId, NoteId, ProjectId, SessionId};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MemorySource {
    Person { session: SessionId },
    Agent { invocation: InvocationId },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MemoryState {
    Pending,
    Active,
    Rejected,
    Superseded { by: NoteId },
    Forgotten,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectMemory {
    pub id: NoteId,
    pub project: ProjectId,
    pub topic: String,
    pub body: String,
    pub source: MemorySource,
    pub state: MemoryState,
    #[serde(default)]
    pub replaces: Option<NoteId>,
    #[serde(default)]
    pub reviewed_by: Option<SessionId>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}
