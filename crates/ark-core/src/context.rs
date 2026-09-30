//! The workspace context: a compact, structured description of *what the user is working
//! within right now*, so an on-screen reference ("what does this do?", "summarize this",
//! "anything interesting here?") resolves without the user restating it.
//!
//! [`Focus`] (in [`crate::surface`]) is the raw thing a UI reports — the focused surface
//! plus any selection. [`WorkspaceContext`] is the *assembled* view the runtime reasons
//! over: the project, the focus, the concrete artifact the user is looking at (resolved
//! from the focus), an active session label, and a few compact references to nearby
//! evidence. It is built by the runtime against the store (core stays storage-free),
//! rendered to a small bounded digest, and — crucially — fed through the same context
//! budget as everything else, never bolted on unbudgeted.
//!
//! Like the rest of core it names no tool and no domain: every field is a data label.

use crate::ids::ArtifactId;
use crate::surface::Focus;
use serde::{Deserialize, Serialize};

/// The assembled description of the current workspace, ready to render into a prompt.
/// Everything is optional so a bare "agent-only" workspace produces an empty context
/// (and no digest), while a viewer-with-selection produces a rich one.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct WorkspaceContext {
    /// The project, as `name — target` (or just the name). Kept short.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub project: String,
    /// What the user is looking at (focused surface + selection).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus: Option<Focus>,
    /// The concrete artifact the focus resolves to, when the surface shows one (a viewer
    /// over an artifact, or an editor of a file that was ingested). This is what lets
    /// "this" become an `artifact` argument.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<ArtifactId>,
    /// A label for an active session the user is in (a terminal, a live session), so "run
    /// it again here" has a target. A data label, no tool name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// Compact one-line references to evidence near the focus. Reference lines only — the
    /// full rows stay retrievable on demand, so this never blows the budget.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nearby_evidence: Vec<String>,
}

impl WorkspaceContext {
    pub fn new(project: impl Into<String>) -> Self {
        WorkspaceContext {
            project: project.into(),
            ..Default::default()
        }
    }

    pub fn with_focus(mut self, focus: Option<Focus>) -> Self {
        self.focus = focus.filter(|f| !f.is_empty());
        self
    }

    /// Is there anything worth telling the agent? A context with only a project name and
    /// no focus/artifact/evidence is not worth a prompt block (the project is already in
    /// the static system prompt).
    pub fn is_empty(&self) -> bool {
        self.focus.as_ref().map(|f| f.is_empty()).unwrap_or(true)
            && self.artifact.is_none()
            && self.session.is_none()
            && self.nearby_evidence.is_empty()
    }

    /// The single most relevant artifact id as a string, for the agent to pass as an
    /// `artifact` argument. Prefers the resolved artifact, then a focus surface's own.
    pub fn active_artifact(&self) -> Option<&ArtifactId> {
        self.artifact.as_ref().or_else(|| {
            self.focus
                .as_ref()
                .and_then(|f| f.surface.as_ref())
                .and_then(|s| s.artifact.as_ref())
        })
    }

    /// A compact, budgeted digest for the prompt, or `None` when nothing meaningful is in
    /// context. `max_evidence` caps the nearby-evidence lines so the block stays bounded
    /// no matter how much evidence a busy project holds.
    pub fn digest(&self, max_evidence: usize) -> Option<String> {
        if self.is_empty() {
            return None;
        }
        use std::fmt::Write as _;
        let mut s = String::new();
        s.push_str(
            "The user is working in a live workspace. Resolve on-screen references \
                    (\"this\", \"here\", \"it\") against what they are looking at:\n",
        );

        if let Some(focus) = &self.focus {
            if let Some(d) = focus.digest() {
                let _ = writeln!(s, "- {d}");
            }
        }
        if let Some(a) = self.active_artifact() {
            let _ = writeln!(
                s,
                "- The focused artifact id is {} — pass it as the `artifact` argument for a tool call.",
                a.as_str()
            );
        }
        if let Some(sess) = &self.session {
            let _ = writeln!(s, "- Active session: {sess}.");
        }
        if !self.nearby_evidence.is_empty() {
            let _ = writeln!(
                s,
                "- Evidence already gathered near this focus (retrieve rows with get_evidence):"
            );
            for line in self.nearby_evidence.iter().take(max_evidence) {
                let _ = writeln!(s, "    {line}");
            }
            if self.nearby_evidence.len() > max_evidence {
                let _ = writeln!(
                    s,
                    "    … (+{} more — search_evidence to find them)",
                    self.nearby_evidence.len() - max_evidence
                );
            }
        }
        Some(s.trim_end().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::surface::{Selection, SurfaceKind, SurfaceRef};

    #[test]
    fn empty_context_has_no_digest() {
        assert!(WorkspaceContext::default().digest(5).is_none());
        // A project name alone is not enough to be worth a block.
        assert!(WorkspaceContext::new("chapter 3 — the reveal").is_empty());
    }

    #[test]
    fn digest_resolves_this_to_the_focused_artifact() {
        let ctx = WorkspaceContext::new("novel").with_focus(Some(Focus {
            surface: Some(SurfaceRef {
                kind_tag: SurfaceKind::Editor.as_tag(),
                title: "chapter3.md".into(),
                artifact: Some(ArtifactId("abc123def456".into())),
                path: Some("chapter3.md".into()),
                locator: None,
            }),
            selection: Some(Selection {
                line: Some(42),
                ..Default::default()
            }),
        }));
        let d = ctx.digest(5).unwrap();
        assert!(d.contains("editor surface"), "{d}");
        assert!(
            d.contains("abc123def456"),
            "resolves the artifact id for `this`"
        );
        assert!(d.contains("line 42"));
        assert_eq!(ctx.active_artifact().unwrap().as_str(), "abc123def456");
    }

    #[test]
    fn nearby_evidence_is_capped() {
        let mut ctx = WorkspaceContext::new("p");
        ctx.artifact = Some(ArtifactId("deadbeef".into()));
        for i in 0..10 {
            ctx.nearby_evidence
                .push(format!("[inv:{i:08x}] search — {i} hits"));
        }
        let d = ctx.digest(3).unwrap();
        assert!(d.contains("(+7 more"), "evidence capped at 3 of 10:\n{d}");
        assert!(!ctx.is_empty());
    }
}
