//! Rule sets: a project's standards (how it writes, reviews, tests, names things) kept once,
//! versioned, and applied to the agents and tasks they are bound to. ARK holds the mechanism;
//! the rules themselves are the application's or the project's.
//!
//! A rule set is written as plain text, one rule per list item:
//!
//! ```text
//! # Engineering rules
//! - [E1] (always) Never commit a secret. #security
//! - [E2] Every public function has a test. #testing #code
//! - Prefer small pull requests. #review
//! ```
//!
//! `[id]` names a rule (one is generated when missing), `(always)` makes it apply to every
//! task it is bound to, and `#tags` help select it for a task. Selection and binding live in
//! the runtime (`ark_runtime::rules`); these are the stored types.

use serde::{Deserialize, Serialize};

/// One rule of a set.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    /// Stable within its set (`E12`).
    pub id: String,
    pub text: String,
    /// Words that mark the tasks it is relevant to.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Applies to every task the set is bound to, relevant or not.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub always: bool,
}

impl Rule {
    pub fn new(id: impl Into<String>, text: impl Into<String>) -> Self {
        Rule {
            id: id.into(),
            text: text.into(),
            tags: Vec::new(),
            always: false,
        }
    }
    pub fn tags<S: Into<String>>(mut self, tags: impl IntoIterator<Item = S>) -> Self {
        self.tags = tags.into_iter().map(Into::into).collect();
        self
    }
    pub fn always(mut self) -> Self {
        self.always = true;
        self
    }
}

/// A named, versioned set of rules.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleSet {
    pub name: String,
    /// Assigned by the store: 1 for the first version, +1 for each changed one.
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub description: String,
    pub rules: Vec<Rule>,
}

impl RuleSet {
    pub fn new(name: impl Into<String>) -> Self {
        RuleSet {
            name: name.into(),
            version: 0,
            description: String::new(),
            rules: Vec::new(),
        }
    }
    pub fn describe(mut self, d: impl Into<String>) -> Self {
        self.description = d.into();
        self
    }
    pub fn rule(mut self, r: Rule) -> Self {
        self.rules.push(r);
        self
    }

    /// Parse the plain text form (see the module docs). Lines that are not list items are
    /// ignored, except a first `# heading`, which becomes the description.
    pub fn parse(name: impl Into<String>, text: &str) -> Self {
        let mut set = RuleSet::new(name);
        for line in text.lines() {
            let t = line.trim();
            if set.rules.is_empty() && set.description.is_empty() {
                if let Some(h) = t.strip_prefix('#').filter(|h| h.starts_with(' ')) {
                    set.description = h.trim().to_string();
                    continue;
                }
            }
            let Some(body) = list_item(t) else { continue };
            let mut body = body.trim();
            let mut id = None;
            if let Some(rest) = body.strip_prefix('[') {
                if let Some((inner, after)) = rest.split_once(']') {
                    id = Some(inner.trim().to_string());
                    body = after.trim();
                }
            }
            let mut always = false;
            if let Some(rest) = body.strip_prefix("(always)") {
                always = true;
                body = rest.trim();
            }
            let mut tags = Vec::new();
            let mut words = Vec::new();
            for w in body.split_whitespace() {
                match w.strip_prefix('#') {
                    Some(tag) if !tag.is_empty() => tags.push(tag.to_lowercase()),
                    _ => words.push(w),
                }
            }
            let text = words.join(" ");
            if text.is_empty() {
                continue;
            }
            let id = id
                .filter(|i| !i.is_empty())
                .unwrap_or_else(|| format!("R{}", set.rules.len() + 1));
            set.rules.push(Rule {
                id,
                text,
                tags,
                always,
            });
        }
        set
    }

    /// A digest of the content (not the version), to tell whether a stored set changed.
    pub fn digest(&self) -> String {
        let body = serde_json::json!({ "description": self.description, "rules": self.rules });
        blake3::hash(body.to_string().as_bytes()).to_hex()[..16].to_string()
    }

    pub fn get(&self, id: &str) -> Option<&Rule> {
        self.rules.iter().find(|r| r.id.eq_ignore_ascii_case(id))
    }
}

/// `- x`, `* x`, `12. x`, `12) x` → `x`.
fn list_item(t: &str) -> Option<&str> {
    if let Some(r) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) {
        return Some(r);
    }
    let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0 {
        let rest = &t[digits..];
        return rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") "));
    }
    None
}

/// Where a rule set applies. Agent and task ids are the orchestration layer's.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scope", content = "id", rename_all = "snake_case")]
pub enum RuleScope {
    /// Every agent and task in the project, and a single agent's turns.
    Project(String),
    Agent(String),
    Task(String),
}

/// A rule set attached to a scope, at a pinned version or the latest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleBinding {
    pub ruleset: String,
    /// `None`: whatever version is newest when a task starts.
    #[serde(default)]
    pub version: Option<u32>,
    pub scope: RuleScope,
}

impl RuleBinding {
    pub fn new(ruleset: impl Into<String>, scope: RuleScope) -> Self {
        RuleBinding {
            ruleset: ruleset.into(),
            version: None,
            scope,
        }
    }
    pub fn pinned(mut self, version: u32) -> Self {
        self.version = Some(version);
        self
    }
}

/// One rule as applied to a run: which set, which version, which rule. Recorded so a trace
/// shows which rules shaped an execution.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RuleRef {
    pub set: String,
    pub version: u32,
    pub id: String,
}

impl std::fmt::Display for RuleRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}@{}:{}", self.set, self.version, self.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ids_tags_and_always() {
        let set = RuleSet::parse(
            "eng",
            "# Engineering rules\n\nIntro text is ignored.\n- [E1] (always) Never commit a secret. #secrets\n- Every public function has a test. #testing #code\n3. Prefer small changes.\n- #only-a-tag\n",
        );
        assert_eq!(set.description, "Engineering rules");
        assert_eq!(set.rules.len(), 3);
        assert_eq!(set.rules[0].id, "E1");
        assert!(set.rules[0].always);
        assert_eq!(set.rules[0].tags, vec!["secrets"]);
        assert_eq!(set.rules[1].id, "R2");
        assert_eq!(set.rules[1].text, "Every public function has a test.");
        assert_eq!(set.rules[1].tags, vec!["testing", "code"]);
        assert_eq!(set.rules[2].text, "Prefer small changes.");
        assert!(set.get("e1").is_some());
    }

    #[test]
    fn digest_follows_content_not_version() {
        let a = RuleSet::new("x").rule(Rule::new("1", "a"));
        let mut b = a.clone();
        b.version = 7;
        assert_eq!(a.digest(), b.digest());
        let c = a.clone().rule(Rule::new("2", "b"));
        assert_ne!(a.digest(), c.digest());
    }
}
