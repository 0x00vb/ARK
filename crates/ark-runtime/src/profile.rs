//! The agent profile: the single place a consuming application injects its domain into the
//! otherwise domain-agnostic runtime. Everything security-specific in the source workstation
//! — the "You are Workbench, a security-engineering partner" persona, the fixed capability
//! taxonomy, the RE-specific result distillers, the built-in knowledge base — is now a field
//! here, supplied by the consumer. The runtime owns the *mechanics* (the loop, the budget,
//! the approval gate, the evidence model); the profile owns the *domain*.

use std::sync::Arc;

use ark_core::CapabilityCatalog;

use crate::acquire::AcquisitionProvider;
use crate::approval::ApprovalClassifier;
use crate::capmap::{declared_only, Inferrer};
use crate::distill::DistillerSet;
use crate::knowledge::KnowledgeProvider;

/// The prompt fragments the runtime composes around a task. Defaults are domain-neutral; a
/// consumer overrides any of them to shape the agent's voice and method.
#[derive(Clone)]
pub struct Prompts {
    /// The persona + method system block — the heart of the agent's identity. This is what
    /// was hard-coded as the security persona in the source; here it is entirely yours. The
    /// runtime appends only a mechanical footer (the project label, artifact list, and
    /// approval note).
    pub instructions: String,
    /// The planner's system prompt (used when the planner is enabled and the task is not
    /// trivial). Should ask for a short, domain-neutral plan.
    pub plan_system: String,
    /// The critic's revision instruction, appended after the persona when a draft answer
    /// fails the self-check.
    pub critic_intro: String,
    /// The nudge that forces a final synthesis when the step budget is exhausted.
    pub force_answer: String,
}

impl Default for Prompts {
    fn default() -> Self {
        Prompts {
            instructions: DEFAULT_INSTRUCTIONS.to_string(),
            plan_system: DEFAULT_PLAN_SYSTEM.to_string(),
            critic_intro: DEFAULT_CRITIC_INTRO.to_string(),
            force_answer: DEFAULT_FORCE_ANSWER.to_string(),
        }
    }
}

/// Everything a consuming application supplies to make the runtime *its* agent. Build one
/// with [`AgentProfile::builder`] or start from [`AgentProfile::default`] (a generic,
/// grounded assistant with no domain tools).
#[derive(Clone)]
pub struct AgentProfile {
    pub prompts: Prompts,
    /// The capability taxonomy the agent reasons in. Empty → the runtime falls back to a
    /// flat adapter listing.
    pub capabilities: CapabilityCatalog,
    /// Maps action names → capabilities when a descriptor doesn't declare them.
    pub inferrer: Inferrer,
    /// The result distillers (domain families + the built-in common distiller and fallback).
    pub distillers: DistillerSet,
    /// An optional external-knowledge source; enables the `search_knowledge` meta-tool.
    pub knowledge: Option<Arc<dyn KnowledgeProvider>>,
    /// An optional capability-acquisition source; enables the `find_capability` /
    /// `propose_capability` / `acquire_capability` meta-tools.
    pub acquisition: Option<Arc<dyn AcquisitionProvider>>,
    /// Actions kept always-loaded so a simple task needs no discovery turn.
    pub core_actions: Vec<String>,
    /// Phrases that mark a question as a full investigation (vs. trivial single-step). The
    /// defaults are domain-neutral; a domain may add its own (e.g. "vulnerability").
    pub investigation_markers: Vec<String>,
    /// Maps a state-changing call to its approval class, the key an "allow for this session"
    /// grant is remembered under. `None` classes by action name.
    pub approval_classifier: Option<ApprovalClassifier>,
}

impl Default for AgentProfile {
    fn default() -> Self {
        AgentProfile {
            prompts: Prompts::default(),
            capabilities: CapabilityCatalog::default(),
            inferrer: declared_only(),
            distillers: DistillerSet::default(),
            knowledge: None,
            acquisition: None,
            core_actions: Vec::new(),
            investigation_markers: default_markers(),
            approval_classifier: None,
        }
    }
}

impl AgentProfile {
    pub fn builder() -> AgentProfileBuilder {
        AgentProfileBuilder {
            profile: AgentProfile::default(),
        }
    }

    /// The approval class for a call (see [`AgentProfile::approval_classifier`]).
    pub fn approval_class(&self, action: &str, input: &serde_json::Value) -> String {
        match &self.approval_classifier {
            Some(f) => f(action, input),
            None => action.to_string(),
        }
    }
}

/// Fluent builder for [`AgentProfile`].
pub struct AgentProfileBuilder {
    profile: AgentProfile,
}

impl AgentProfileBuilder {
    /// Set the persona + method block — the agent's identity.
    pub fn instructions(mut self, s: impl Into<String>) -> Self {
        self.profile.prompts.instructions = s.into();
        self
    }
    pub fn prompts(mut self, p: Prompts) -> Self {
        self.profile.prompts = p;
        self
    }
    pub fn capabilities(mut self, c: CapabilityCatalog) -> Self {
        self.profile.capabilities = c;
        self
    }
    pub fn inferrer(mut self, i: Inferrer) -> Self {
        self.profile.inferrer = i;
        self
    }
    pub fn distillers(mut self, d: DistillerSet) -> Self {
        self.profile.distillers = d;
        self
    }
    pub fn knowledge(mut self, k: Arc<dyn KnowledgeProvider>) -> Self {
        self.profile.knowledge = Some(k);
        self
    }
    pub fn acquisition(mut self, a: Arc<dyn AcquisitionProvider>) -> Self {
        self.profile.acquisition = Some(a);
        self
    }
    pub fn core_actions<S: Into<String>>(mut self, actions: impl IntoIterator<Item = S>) -> Self {
        self.profile.core_actions = actions.into_iter().map(Into::into).collect();
        self
    }
    pub fn investigation_markers<S: Into<String>>(
        mut self,
        m: impl IntoIterator<Item = S>,
    ) -> Self {
        self.profile.investigation_markers = m.into_iter().map(Into::into).collect();
        self
    }
    /// Class state-changing calls for session grants, e.g. by the first word of a command.
    pub fn approval_classifier(
        mut self,
        f: impl Fn(&str, &serde_json::Value) -> String + Send + Sync + 'static,
    ) -> Self {
        self.profile.approval_classifier = Some(Arc::new(f));
        self
    }
    pub fn build(self) -> AgentProfile {
        self.profile
    }
}

fn default_markers() -> Vec<String> {
    [
        "why",
        "how does",
        "how is",
        "compare",
        " and ",
        " then ",
        "explain",
        "investigate",
        "analyze",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

pub const DEFAULT_INSTRUCTIONS: &str =
    "You are a capable agent that gets things done by operating real tools, and you reason like \
a careful professional, not a chatbot.\n\n\
Method:\n\
- Reason about capabilities, not tool names. First decide what kind of information or action \
the task needs; the capability index shows which are available and the actions that serve \
each. Load an action's schema before calling it. A few common actions are already loaded.\n\
- Ground every claim in a tool result. Do not invent a fact — if you need one, call a tool \
to get it.\n\
- Separate what you have established from what you believe. Use record_state to add a \
finding (cite the evidence), state a hypothesis, or note the next step. The state persists \
across turns — build on it rather than repeating work.\n\
- Tool results are compact summaries with an [inv:ID] reference. To see exact rows, call \
get_evidence(invocation, path). Do not re-run a tool you already ran.\n\
- When evidence is missing, run a small experiment and read its result as evidence.\n\
- When you can answer, stop calling tools and reply: separate facts (cited) from inference \
and hypothesis, and end with the single most useful next step.";

pub const DEFAULT_PLAN_SYSTEM: &str =
    "You are planning how to accomplish a task. Given the goal, output a SHORT plan of 3–6 \
concrete steps, one per line, no numbering, phrased as capabilities to use. Let the goal \
decide the steps — do not assume any particular tool or workflow unless the goal implies it. \
Output only the steps.";

pub const DEFAULT_CRITIC_INTRO: &str =
    "You are reviewing a draft answer that failed a check. Revise it to be honest and grounded: \
cite evidence for factual claims, and where evidence is missing say so and label the claim a \
hypothesis or unknown. Output only the revised answer.";

pub const DEFAULT_FORCE_ANSWER: &str =
    "Stop calling tools. Give your best final answer from the evidence gathered, clearly \
separating established facts (with [inv:…] citations) from hypotheses, and end with the \
single most useful next step.";
