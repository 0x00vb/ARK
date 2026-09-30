//! # ark-core
//!
//! The domain-agnostic heart of ARK (Agent Runtime Kit). Pure types and one invariant
//! that keeps the whole system reusable across unrelated domains:
//!
//! **No tool name and no domain name may appear in this crate.** Not `iOS`, not `Ghidra`,
//! not `manuscript`, not `contract`. Those live only in adapters and consumer
//! configuration. This is what makes new domains and new tools *additive* rather than
//! rewrites — a security product, a writing assistant, and a legal researcher all build on
//! the exact same core.
//!
//! The abstractions:
//! - [`Project`] — one working target and its workspace.
//! - [`Artifact`] — any object of study or authorship, content-addressed and
//!   provenance-tracked.
//! - [`ActionDescriptor`] / [`RiskTier`] — a typed capability an adapter offers, and how
//!   consequential it is.
//! - [`Invocation`] — the recorded execution of an action (the reproducibility spine).
//! - [`Capability`] / [`CapabilityCatalog`] — the tool-neutral vocabulary the agent
//!   reasons in, supplied by the consuming application.
//! - [`EvidenceCard`] / [`ResearchState`] — the compact evidence and working-memory model
//!   that keeps the agent's context small and its answers cited.
//! - [`WorkspaceContext`] / [`Focus`] / [`PresentDirective`] — what the user is looking at,
//!   and what the agent asks a UI to show.

pub mod action;
pub mod artifact;
pub mod capability;
pub mod context;
pub mod error;
pub mod ids;
pub mod invocation;
pub mod project;
pub mod research;
pub mod surface;

pub use action::{ActionDescriptor, RiskTier};
pub use artifact::{Artifact, ArtifactKind, Provenance};
pub use capability::{Capability, CapabilityCatalog, CapabilitySpec};
pub use context::WorkspaceContext;
pub use error::{CoreError, Result};
pub use ids::{ArtifactId, InvocationId, NoteId, ProjectId, SessionId};
pub use invocation::{Invocation, InvocationStatus};
pub use project::{Author, Note, NoteKind, Project};
pub use research::{
    Claim, ClaimType, EdgeRel, EvidenceCard, GraphEdge, GraphNode, Interesting, InvestigationGraph,
    NodeKind, ResearchState,
};
pub use surface::{Focus, PresentDirective, Selection, SurfaceKind, SurfaceRef};

/// A pluggable content sniffer: classify raw bytes into an [`ArtifactKind`] on ingest.
/// ARK ships only [`detect_kind`] (text/json/binary); a domain registers a sniffer that
/// recognizes its formats (Mach-O, PDF, a manuscript container, …) without a core change.
pub trait Sniffer: Send + Sync {
    /// Return a kind if these bytes are recognized, else `None` to defer to the next
    /// sniffer (and finally to [`detect_kind`]).
    fn sniff(&self, name: &str, bytes: &[u8]) -> Option<ArtifactKind>;
}

/// The built-in, domain-free content classifier. Recognizes JSON and UTF-8 text; anything
/// else is [`ArtifactKind::Binary`]. Domains layer richer detection via [`Sniffer`].
pub fn detect_kind(bytes: &[u8]) -> ArtifactKind {
    if bytes.is_empty() {
        return ArtifactKind::Blob;
    }
    if let Ok(s) = std::str::from_utf8(bytes) {
        let trimmed = s.trim_start();
        if (trimmed.starts_with('{') || trimmed.starts_with('['))
            && serde_json::from_str::<serde_json::Value>(trimmed).is_ok()
        {
            return ArtifactKind::Json;
        }
        return ArtifactKind::Text;
    }
    ArtifactKind::Binary
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_id_is_content_addressed() {
        let a = ArtifactId::of_bytes(b"hello world");
        let b = ArtifactId::of_bytes(b"hello world");
        let c = ArtifactId::of_bytes(b"goodbye");
        assert_eq!(a, b, "same bytes -> same id");
        assert_ne!(a, c, "different bytes -> different id");
        assert_eq!(a.short().len(), 12);
    }

    #[test]
    fn risk_tier_gates_auto_run() {
        assert!(RiskTier::Read.auto_runs());
        assert!(RiskTier::Observe.auto_runs());
        assert!(!RiskTier::Mutate.auto_runs(), "mutations need approval");
    }

    #[test]
    fn mutate_invocation_starts_pending() {
        let inv = Invocation::new("fs.write", RiskTier::Mutate, serde_json::json!({}));
        assert_eq!(inv.status, InvocationStatus::PendingApproval);

        let inv = Invocation::new("fs.read", RiskTier::Read, serde_json::json!({}));
        assert_eq!(inv.status, InvocationStatus::Running);
    }

    #[test]
    fn action_name_splits_adapter() {
        let d = ActionDescriptor::new(
            "draft.expand",
            "expand an outline into prose",
            RiskTier::Read,
        );
        assert_eq!(d.adapter_name(), "draft");
    }

    #[test]
    fn detect_kind_classifies_text_json_binary() {
        assert_eq!(detect_kind(b"hello text"), ArtifactKind::Text);
        assert_eq!(detect_kind(b"{\"a\":1}"), ArtifactKind::Json);
        assert_eq!(detect_kind(&[0xff, 0xfe, 0x00, 0x01]), ArtifactKind::Binary);
        assert_eq!(detect_kind(b""), ArtifactKind::Blob);
    }

    #[test]
    fn invocation_roundtrips_json() {
        let mut inv = Invocation::new("fs.read", RiskTier::Read, serde_json::json!({"x":1}));
        inv.mark_ok(serde_json::json!({"lines":3}));
        let s = serde_json::to_string(&inv).unwrap();
        let back: Invocation = serde_json::from_str(&s).unwrap();
        assert_eq!(back.status, InvocationStatus::Ok);
        assert_eq!(back.output["lines"], 3);
    }
}
