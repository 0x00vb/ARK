//! Groups of agents (ADR 0113, ADR 0117): the record, and what a group's membership means.
//!
//! Membership is one field: [`crate::AgentSpec::groups`] holds the ids of the groups an agent
//! is in. Contact between members and the group's rules derive from it, so removing the id
//! ends both.

use std::collections::BTreeMap;

use ark_core::{Rule, RuleSet};
use serde::{Deserialize, Serialize};

/// The store scope of group records.
pub const SCOPE: &str = "#groups";
/// The store kind of group records.
pub const RECORD: &str = "group";

/// Rules one group holds at most, and the longest rule, in characters.
pub const MAX_RULES: usize = 20;
pub const RULE_CHARS: usize = 300;
/// Always-rules one agent may follow across all its groups.
pub const MAX_AGENT_RULES: usize = 20;
/// The longest group name, in characters.
pub const NAME_CHARS: usize = 80;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Member {
    pub project: String,
    pub agent: String,
}

impl Member {
    pub fn new(project: impl Into<String>, agent: impl Into<String>) -> Self {
        Member {
            project: project.into(),
            agent: agent.into(),
        }
    }

    /// The key of the group's `threads` and `cursors` maps.
    pub fn key(&self) -> String {
        format!("{}/{}", self.project, self.agent)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Group {
    pub id: String,
    pub name: String,
    pub members: Vec<Member>,
    /// Member key to the thread it answers in for this group.
    #[serde(default)]
    pub threads: BTreeMap<String, String>,
    /// Member key to the last message id its brief included.
    #[serde(default)]
    pub cursors: BTreeMap<String, String>,
    pub created_ms: u64,
    pub updated_ms: u64,
}

/// The rule set a group's members follow.
pub fn rule_set_name(group: &str) -> String {
    format!("group-{group}")
}

/// A group's rules as a rule set: every rule applies to every task of every member.
pub fn rule_set(group: &str, name: &str, rules: &[String]) -> RuleSet {
    rules.iter().enumerate().fold(
        RuleSet::new(rule_set_name(group)).describe(name),
        |set, (i, text)| set.rule(Rule::new(format!("R{}", i + 1), text.clone()).always()),
    )
}

/// One member of a `groups.set` call. `role`, `personality` and `selection` shape a member
/// that does not exist yet; an agent of this folder keeps its own.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct Candidate {
    pub name: String,
    pub role: String,
    pub personality: String,
    pub selection: crate::execution::ExecutionSelection,
}

/// A `groups.set` call, checked for shape.
#[derive(Clone, Debug)]
pub(crate) struct Request {
    pub name: String,
    /// The group's full list of rules. `None`: keep the rules it has.
    pub rules: Option<Vec<String>>,
    pub members: Vec<Candidate>,
}

impl Request {
    /// The id of a group made by this call: the same name in the same folder is the same
    /// group, so a later call updates it and a retry finds what the first one wrote.
    pub fn id(&self, project: &str) -> String {
        let digest =
            blake3::hash(format!("{project}\n{}", self.name.to_lowercase()).as_bytes()).to_hex();
        format!("g-{}", &digest[..20])
    }
}

/// What a `groups.set` call did.
#[derive(Debug, Serialize)]
pub(crate) struct Outcome {
    pub group: String,
    pub name: String,
    /// Every member of the group after the call.
    pub members: Vec<String>,
    /// Agents made by this call.
    pub created: Vec<String>,
    /// Agents that already existed and joined it, keeping their role and personality.
    pub joined: Vec<String>,
    /// The call changed the group's rules.
    pub rules_changed: bool,
}
