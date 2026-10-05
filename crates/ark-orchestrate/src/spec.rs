//! What an application declares: agents ([`AgentSpec`]) and the work it hands them
//! ([`TaskSpec`]), each with an optional [`Budget`].

use ark_core::RuleBinding;
use ark_runtime::AgentProfile;
use serde::{Deserialize, Serialize};

/// A logical agent: its identity, instructions, tools, permissions and model. Each agent
/// gets its own context (a child project of the orchestrator's project) and its own model
/// session.
#[derive(Clone, Serialize, Deserialize)]
pub struct AgentSpec {
    /// Unique within the orchestrator: how other agents and the graph name it.
    pub name: String,
    /// One line on what it does, shown to agents that may delegate to it.
    #[serde(default)]
    pub role: String,
    /// Its persona and method. Empty: the orchestrator's base profile instructions.
    #[serde(default)]
    pub instructions: String,
    /// `provider:model` (`claude:sonnet`, `codex`, `ollama:qwen3:4b`). `None`: the
    /// orchestrator's default model.
    #[serde(default)]
    pub model: Option<String>,
    /// Patterns of the application's actions it may use (`fs.read`, `fs.*`, `*`). Empty:
    /// none.
    #[serde(default = "all_tools")]
    pub tools: Vec<String>,
    /// Agents it may delegate to and message (`*`: any). Its parent may always be messaged.
    #[serde(default)]
    pub delegates_to: Vec<String>,
    /// Run state-changing actions without asking. Off: they go to the orchestrator's
    /// approval gate, or are refused when there is none.
    #[serde(default)]
    pub allow_mutate: bool,
    /// The agent this one belongs to, for a hierarchy. Topology only; permissions are
    /// `delegates_to`.
    #[serde(default)]
    pub parent: Option<String>,
    /// The budget of its tasks when the task sets none.
    #[serde(default)]
    pub budget: Budget,
    /// Offer the diagram tool (`diagram.create`, `diagram.read`).
    #[serde(default)]
    pub diagrams: bool,
    /// The skills it may read (`*`: all, else names; empty: none). Their catalog rides in each
    /// task's first message; their instructions load with `skills.read`
    /// ([`ark_runtime::skills`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
    /// Override the orchestrator's planner and critic settings for this agent.
    #[serde(default)]
    pub planner: Option<bool>,
    #[serde(default)]
    pub critic: Option<bool>,
    /// A profile the application registered with
    /// [`OrchestratorBuilder::profile`](crate::OrchestratorBuilder::profile), by name. Unlike
    /// `profile` it is stored, so a front-end can choose it and it survives a restart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_name: Option<String>,
    /// A full profile (capabilities, distillers, knowledge) instead of the orchestrator's
    /// base profile. `instructions` still replaces its persona when set. Not persisted.
    #[serde(skip)]
    pub profile: Option<AgentProfile>,
}

impl std::fmt::Debug for AgentSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentSpec")
            .field("name", &self.name)
            .field("role", &self.role)
            .field("model", &self.model)
            .field("tools", &self.tools)
            .field("delegates_to", &self.delegates_to)
            .field("parent", &self.parent)
            .field("profile_name", &self.profile_name)
            .field("skills", &self.skills)
            .finish_non_exhaustive()
    }
}

fn all_tools() -> Vec<String> {
    vec!["*".into()]
}

impl AgentSpec {
    pub fn new(name: impl Into<String>) -> Self {
        AgentSpec {
            name: name.into(),
            role: String::new(),
            instructions: String::new(),
            model: None,
            tools: all_tools(),
            delegates_to: Vec::new(),
            allow_mutate: false,
            parent: None,
            budget: Budget::default(),
            diagrams: false,
            planner: None,
            critic: None,
            profile_name: None,
            profile: None,
            skills: Vec::new(),
        }
    }
    pub fn role(mut self, r: impl Into<String>) -> Self {
        self.role = r.into();
        self
    }
    pub fn instructions(mut self, i: impl Into<String>) -> Self {
        self.instructions = i.into();
        self
    }
    pub fn model(mut self, spec: impl Into<String>) -> Self {
        self.model = Some(spec.into());
        self
    }
    /// The application actions it may use (patterns, see [`ark_tools::action_matches`]).
    pub fn tools<S: Into<String>>(mut self, patterns: impl IntoIterator<Item = S>) -> Self {
        self.tools = patterns.into_iter().map(Into::into).collect();
        self
    }
    pub fn delegates_to<S: Into<String>>(mut self, agents: impl IntoIterator<Item = S>) -> Self {
        self.delegates_to = agents.into_iter().map(Into::into).collect();
        self
    }
    /// The skills it may read: names, or `*` for every skill of the project and the store.
    pub fn skills<S: Into<String>>(mut self, names: impl IntoIterator<Item = S>) -> Self {
        self.skills = names.into_iter().map(Into::into).collect();
        self
    }
    pub fn allow_mutate(mut self, yes: bool) -> Self {
        self.allow_mutate = yes;
        self
    }
    pub fn parent(mut self, p: impl Into<String>) -> Self {
        self.parent = Some(p.into());
        self
    }
    pub fn budget(mut self, b: Budget) -> Self {
        self.budget = b;
        self
    }
    pub fn diagrams(mut self, yes: bool) -> Self {
        self.diagrams = yes;
        self
    }
    pub fn planner(mut self, yes: bool) -> Self {
        self.planner = Some(yes);
        self
    }
    pub fn critic(mut self, yes: bool) -> Self {
        self.critic = Some(yes);
        self
    }
    pub fn profile(mut self, p: AgentProfile) -> Self {
        self.profile = Some(p);
        self
    }
    /// Start from the profile the application registered under `name`.
    pub fn profile_named(mut self, name: impl Into<String>) -> Self {
        self.profile_name = Some(name.into());
        self
    }

    /// Whether it may delegate to or message `other`.
    pub fn may_contact(&self, other: &str) -> bool {
        other != self.name
            && (self.parent.as_deref() == Some(other)
                || self.delegates_to.iter().any(|d| d == "*" || d == other))
    }

    /// Whether it may hand tasks to `other`.
    pub fn may_delegate(&self, other: &str) -> bool {
        other != self.name && self.delegates_to.iter().any(|d| d == "*" || d == other)
    }

    /// Whether it may delegate at all.
    pub fn delegates(&self) -> bool {
        !self.delegates_to.is_empty()
    }
}

/// Limits on one task. Unset limits are not enforced.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Budget {
    /// Reasoning steps (the harness forces an answer at the limit).
    #[serde(default)]
    pub max_steps: Option<usize>,
    /// Prompt plus completion tokens across the task's model calls. Passing it stops the
    /// task and fails it.
    #[serde(default)]
    pub max_tokens: Option<u64>,
    #[serde(default)]
    pub max_model_calls: Option<u32>,
    /// Wall time, in seconds, while running (not while queued).
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    /// Tasks it may delegate.
    #[serde(default)]
    pub max_delegations: Option<u32>,
}

impl Budget {
    pub fn steps(mut self, n: usize) -> Self {
        self.max_steps = Some(n);
        self
    }
    pub fn tokens(mut self, n: u64) -> Self {
        self.max_tokens = Some(n);
        self
    }
    pub fn model_calls(mut self, n: u32) -> Self {
        self.max_model_calls = Some(n);
        self
    }
    pub fn timeout_secs(mut self, s: u64) -> Self {
        self.timeout_secs = Some(s);
        self
    }
    pub fn delegations(mut self, n: u32) -> Self {
        self.max_delegations = Some(n);
        self
    }
    /// `self` where set, else `fallback`.
    pub fn or(&self, fallback: &Budget) -> Budget {
        Budget {
            max_steps: self.max_steps.or(fallback.max_steps),
            max_tokens: self.max_tokens.or(fallback.max_tokens),
            max_model_calls: self.max_model_calls.or(fallback.max_model_calls),
            timeout_secs: self.timeout_secs.or(fallback.timeout_secs),
            max_delegations: self.max_delegations.or(fallback.max_delegations),
        }
    }
}

/// A piece of work for one agent: what to achieve and the minimum it needs to know.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TaskSpec {
    pub objective: String,
    /// What the agent needs to know beyond the objective. The agent does not see the
    /// conversation of whoever created the task, only this.
    #[serde(default)]
    pub context: String,
    /// Artifacts (ids) the agent may use. They are added to its context.
    #[serde(default)]
    pub artifacts: Vec<String>,
    /// Tasks that must finish first. Their results are added to the brief.
    #[serde(default)]
    pub after: Vec<String>,
    #[serde(default)]
    pub budget: Option<Budget>,
    /// Attempts when the model or the runtime fails (not when the agent answers). Default 1.
    #[serde(default)]
    pub max_attempts: Option<u32>,
    /// Rule sets bound to this task only.
    #[serde(default)]
    pub rules: Vec<RuleBinding>,
    /// The board item it works on (`w-…`): the item follows the task (see [`crate::board`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
}

impl TaskSpec {
    pub fn new(objective: impl Into<String>) -> Self {
        TaskSpec {
            objective: objective.into(),
            ..Default::default()
        }
    }
    pub fn context(mut self, c: impl Into<String>) -> Self {
        self.context = c.into();
        self
    }
    pub fn artifacts<S: Into<String>>(mut self, ids: impl IntoIterator<Item = S>) -> Self {
        self.artifacts = ids.into_iter().map(Into::into).collect();
        self
    }
    pub fn after<S: Into<String>>(mut self, tasks: impl IntoIterator<Item = S>) -> Self {
        self.after = tasks.into_iter().map(Into::into).collect();
        self
    }
    pub fn budget(mut self, b: Budget) -> Self {
        self.budget = Some(b);
        self
    }
    pub fn attempts(mut self, n: u32) -> Self {
        self.max_attempts = Some(n.max(1));
        self
    }
    pub fn rules(mut self, r: RuleBinding) -> Self {
        self.rules.push(r);
        self
    }
    /// Work on a board item: it moves with this task.
    pub fn item(mut self, id: impl Into<String>) -> Self {
        self.item = Some(id.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contacts_follow_permissions_and_parent() {
        let a = AgentSpec::new("lead").delegates_to(["w1"]);
        assert!(a.may_contact("w1"));
        assert!(!a.may_contact("w2"));
        assert!(!a.may_contact("lead"), "never itself");
        let w = AgentSpec::new("w1").parent("lead");
        assert!(w.may_contact("lead"));
        assert!(!w.delegates());
        assert!(AgentSpec::new("x")
            .delegates_to(["*"])
            .may_contact("anyone"));
    }

    #[test]
    fn budgets_fall_back_field_by_field() {
        let task = Budget::default().tokens(100);
        let agent = Budget::default().tokens(5).steps(3);
        let b = task.or(&agent);
        assert_eq!(b.max_tokens, Some(100));
        assert_eq!(b.max_steps, Some(3));
    }
}
