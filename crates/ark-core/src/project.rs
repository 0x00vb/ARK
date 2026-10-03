//! Projects and notebook entries. A project is one working target.

use crate::ids::{ArtifactId, InvocationId, NoteId, ProjectId};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// A working target and its workspace. Everything else is scoped to a project.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Project {
    pub id: ProjectId,
    pub name: String,
    /// Free-form description of what this project is about (e.g. "acme.example.com audit",
    /// "the second novel", "the Henderson contract review").
    #[serde(default)]
    pub target: String,
    /// An on-disk directory this project's tools operate in (an existing repo/folder, or
    /// scratch space for generated files). When unset, the engine sites a per-project
    /// workspace under the store root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// The project this one belongs to. An orchestrated agent works in a child project of
    /// the project it serves, so its working state, evidence and conversation stay its own.
    /// Front-ends list only top-level projects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<ProjectId>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

impl Project {
    pub fn new(name: impl Into<String>, target: impl Into<String>) -> Self {
        Project {
            id: ProjectId::new(),
            name: name.into(),
            target: target.into(),
            workspace: None,
            parent: None,
            created_at: OffsetDateTime::now_utc(),
        }
    }

    pub fn with_parent(mut self, parent: ProjectId) -> Self {
        self.parent = Some(parent);
        self
    }

    pub fn with_workspace(mut self, workspace: impl Into<String>) -> Self {
        self.workspace = Some(workspace.into());
        self
    }
}

/// A kind of notebook entry, following the research/working loop. The distinctions
/// matter: a good record separates what is *established* (`Fact`, cited to an invocation)
/// from what is *believed* (`Hypothesis`, `Assumption`), and surfaces where evidence
/// disagrees (`Contradiction`) and what to try next (`NextStep`). These are general
/// reasoning categories, not domain-specific ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoteKind {
    /// A proposed explanation, not yet established.
    Hypothesis,
    /// Something taken as true without having verified it here.
    Assumption,
    /// A claim established by cited evidence (an invocation/artifact).
    Fact,
    /// Raw supporting evidence pulled from a tool run.
    Evidence,
    /// A conclusion worth preserving.
    Finding,
    /// Two pieces of evidence that disagree — worth resolving.
    Contradiction,
    /// The next useful step to take.
    NextStep,
    /// An open question.
    Question,
    /// Anything else.
    Note,
}

/// An entry in the notebook. Notes are the human/agent-authored layer on top of the
/// invocation log; together they export to a Markdown record.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Note {
    pub id: NoteId,
    pub kind: NoteKind,
    pub body: String,
    /// Whether a human or the agent wrote this.
    pub author: Author,
    /// Invocations this note draws its evidence from.
    #[serde(default)]
    pub cites_invocations: Vec<InvocationId>,
    #[serde(default)]
    pub cites_artifacts: Vec<ArtifactId>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Author {
    Human,
    Agent,
}

impl Note {
    pub fn new(kind: NoteKind, body: impl Into<String>, author: Author) -> Self {
        Note {
            id: NoteId::new(),
            kind,
            body: body.into(),
            author,
            cites_invocations: Vec::new(),
            cites_artifacts: Vec::new(),
            created_at: OffsetDateTime::now_utc(),
        }
    }
}
