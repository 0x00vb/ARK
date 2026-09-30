//! The capability-*acquisition* seam. Like [`crate::knowledge`], this replaces a
//! domain-specific feature of the source workstation (which could install security tools on
//! demand) with a trait the consumer implements. When a profile supplies an
//! [`AcquisitionProvider`], the runtime exposes the `find_capability` / `propose_capability` /
//! `acquire_capability` meta-tools, so an agent that hits a missing capability can reason
//! about acquiring one — with approval — instead of faking it or giving up.
//!
//! The runtime stays domain-agnostic: it knows only "there may be more capabilities the agent
//! can obtain, gated by approval." *What* those are, and *how* to obtain them, is the
//! consumer's business — a security agent installs a disassembler; a writing agent might wire
//! up a thesaurus service.

/// The result of an acquisition attempt.
#[derive(Clone, Debug)]
pub struct AcquisitionResult {
    /// The human/agent-facing summary of what happened (or what *would* happen if approved).
    pub summary: String,
    /// Whether the environment was actually changed (a provider installed or wired up). The
    /// runtime surfaces this on the event stream so a UI can note a real change occurred.
    pub changed: bool,
}

impl AcquisitionResult {
    /// A read-only result — nothing changed (a plan, a listing, a "needs approval").
    pub fn info(summary: impl Into<String>) -> Self {
        AcquisitionResult { summary: summary.into(), changed: false }
    }
    /// A result that reflects a real environment change (a provider was acquired).
    pub fn changed(summary: impl Into<String>) -> Self {
        AcquisitionResult { summary: summary.into(), changed: true }
    }
}

/// A source of *new* capabilities the agent can obtain at runtime, subject to approval.
///
/// All three methods take a free-text `need` (a capability slug or a natural-language
/// description) so the agent can ask in its own words. `find` and `propose` are read-only;
/// only `acquire` may change the environment, and only when `approved` is true.
///
/// The methods are `async` because acquiring a provider is inherently I/O — probing for a
/// tool, running an installer, connecting a service. Read-only `find`/`propose` may still do
/// I/O (checking what's already present), so they are async too for symmetry.
#[async_trait::async_trait]
pub trait AcquisitionProvider: Send + Sync {
    /// List providers/capabilities that could serve `need` (the `find_capability` meta-tool).
    /// Read-only; returns agent-facing text.
    async fn find(&self, need: &str) -> String;

    /// Describe the acquisition *plan* for `need` without changing anything (the
    /// `propose_capability` meta-tool). Read-only.
    async fn propose(&self, need: &str) -> String;

    /// Attempt to acquire a provider for `need` (the `acquire_capability` meta-tool).
    /// `approved` gates state-changing installs: when false, the implementation must change
    /// nothing and instead return the plan (an [`AcquisitionResult::info`]).
    async fn acquire(&self, need: &str, approved: bool) -> AcquisitionResult;
}
