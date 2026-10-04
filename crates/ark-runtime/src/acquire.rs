//! The capability-*acquisition* seam: a trait the consumer implements, like
//! [`crate::knowledge`]. When a profile supplies an
//! [`AcquisitionProvider`], the runtime exposes the `find_capability` / `propose_capability` /
//! `acquire_capability` meta-tools, so an agent that hits a missing capability can reason
//! about acquiring one — with approval — instead of faking it or giving up.
//!
//! The runtime stays domain-agnostic: it knows only "there may be more capabilities the agent
//! can obtain, gated by approval." *What* those are, and *how* to obtain them, is the
//! consumer's business — a security agent installs a disassembler; a writing agent might wire
//! up a thesaurus service.

/// The result of an acquisition attempt.
#[derive(Clone)]
pub struct AcquisitionResult {
    /// The human/agent-facing summary of what happened (or what *would* happen if approved).
    pub summary: String,
    /// Whether the environment was actually changed (a provider installed or wired up). The
    /// runtime surfaces this on the event stream so a UI can note a real change occurred.
    pub changed: bool,
    /// Tools the acquisition made available. They join the running turn at once, offered
    /// directly, so the agent can go on with the task.
    pub tools: Vec<std::sync::Arc<dyn ark_tools::Adapter>>,
}

impl std::fmt::Debug for AcquisitionResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AcquisitionResult")
            .field("summary", &self.summary)
            .field("changed", &self.changed)
            .field(
                "tools",
                &self
                    .tools
                    .iter()
                    .map(|t| t.name().to_string())
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl AcquisitionResult {
    /// A read-only result — nothing changed (a plan, a listing, a "needs approval").
    pub fn info(summary: impl Into<String>) -> Self {
        AcquisitionResult {
            summary: summary.into(),
            changed: false,
            tools: Vec::new(),
        }
    }
    /// A result that reflects a real environment change (a provider was acquired).
    pub fn changed(summary: impl Into<String>) -> Self {
        AcquisitionResult {
            summary: summary.into(),
            changed: true,
            tools: Vec::new(),
        }
    }

    /// With tools that join the running turn.
    pub fn with_tools(mut self, tools: Vec<std::sync::Arc<dyn ark_tools::Adapter>>) -> Self {
        self.tools = tools;
        self
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

    /// Whether there is anything to acquire right now. When not, the turn is not offered the
    /// acquisition tools. Default: always.
    fn active(&self) -> bool {
        true
    }

    /// A few lines naming what can be acquired, put in a turn's first message so the agent
    /// knows to ask instead of giving up. Default: none.
    fn offer(&self) -> Option<String> {
        None
    }
}
