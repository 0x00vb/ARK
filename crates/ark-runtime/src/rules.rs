//! Rule sets at run time: stored centrally and versioned ([`RuleBook`]), bound to a project,
//! an agent or a task, and applied selectively. A turn never carries a whole standard:
//!
//! 1. [`select`] picks the rules a task needs, deterministically: every `always` rule, then
//!    the rules whose tags and words match the task best, until a token cap is reached.
//! 2. The rest stay one call away through the `rules` tool ([`RulesTool`]): `rules.lookup`
//!    searches the bound sets, `rules.get` reads one rule by id.
//! 3. Each selection is a list of [`RuleRef`]s (set, version, id). The harness reports it as
//!    a [`crate::HarnessEvent::Rules`] event and an orchestrator stores it on the task, so a
//!    trace shows which rules shaped a run.

use std::collections::HashSet;
use std::fmt::Write as _;

use ark_core::{ActionDescriptor, RiskTier, Rule, RuleBinding, RuleRef, RuleScope, RuleSet};
use ark_model::estimate_tokens;
use ark_store::Store;
use ark_tools::{ActionOutput, Adapter, AdapterError, RunCtx};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::AgentError;

const SET: &str = "ruleset";
const BINDING: &str = "rule_binding";

/// Rule sets in the store: each change is a new version; old versions stay readable so a
/// trace that names `eng@3` still resolves.
pub struct RuleBook<'a> {
    store: &'a Store,
}

impl<'a> RuleBook<'a> {
    pub fn new(store: &'a Store) -> Self {
        RuleBook { store }
    }

    /// Store `set`. A set whose content equals the newest stored version is not stored
    /// again. Returns the stored set with its version.
    pub fn put(&self, set: &RuleSet) -> Result<RuleSet, AgentError> {
        let latest = self.latest(&set.name)?;
        if let Some(l) = &latest {
            if l.digest() == set.digest() {
                return Ok(l.clone());
            }
        }
        let mut s = set.clone();
        s.version = latest.map(|l| l.version + 1).unwrap_or(1);
        let id = format!("{}@{}", s.name, s.version);
        self.store
            .put_record(SET, &id, &s.name, &serde_json::to_value(&s)?)?;
        Ok(s)
    }

    /// A set at a version, or its newest version.
    pub fn get(&self, name: &str, version: Option<u32>) -> Result<Option<RuleSet>, AgentError> {
        match version {
            Some(v) => Ok(self
                .store
                .get_record(SET, &format!("{name}@{v}"))?
                .map(serde_json::from_value)
                .transpose()?),
            None => self.latest(name),
        }
    }

    fn latest(&self, name: &str) -> Result<Option<RuleSet>, AgentError> {
        let mut all: Vec<RuleSet> = self
            .store
            .list_records(SET, Some(name))?
            .into_iter()
            .filter_map(|v| serde_json::from_value(v).ok())
            .collect();
        all.sort_by_key(|s| s.version);
        Ok(all.pop())
    }

    /// The newest version of every set.
    pub fn list(&self) -> Result<Vec<RuleSet>, AgentError> {
        let mut newest: std::collections::BTreeMap<String, RuleSet> = Default::default();
        for v in self.store.list_records(SET, None)? {
            let Ok(s) = serde_json::from_value::<RuleSet>(v) else {
                continue;
            };
            if newest.get(&s.name).is_none_or(|n| n.version < s.version) {
                newest.insert(s.name.clone(), s);
            }
        }
        Ok(newest.into_values().collect())
    }

    /// Attach a set to a scope. Binding the same set to the same scope again replaces the
    /// earlier binding (to change the pinned version).
    pub fn bind(&self, binding: &RuleBinding) -> Result<(), AgentError> {
        let scope = scope_key(&binding.scope);
        let id = format!("{scope}|{}", binding.ruleset);
        self.store
            .put_record(BINDING, &id, &scope, &serde_json::to_value(binding)?)?;
        Ok(())
    }

    /// Remove a binding. Returns whether one existed.
    pub fn unbind(&self, ruleset: &str, scope: &RuleScope) -> Result<bool, AgentError> {
        let id = format!("{}|{ruleset}", scope_key(scope));
        Ok(self.store.delete_record(BINDING, &id)?)
    }

    pub fn bindings(&self, scope: &RuleScope) -> Result<Vec<RuleBinding>, AgentError> {
        Ok(self
            .store
            .list_records(BINDING, Some(&scope_key(scope)))?
            .into_iter()
            .filter_map(|v| serde_json::from_value(v).ok())
            .collect())
    }

    /// The sets bound to any of `scopes`, each once (the first binding of a name wins, so
    /// list the narrowest scope first to let a task pin its own version).
    pub fn resolve(&self, scopes: &[RuleScope]) -> Result<Vec<RuleSet>, AgentError> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for scope in scopes {
            for b in self.bindings(scope)? {
                if !seen.insert(b.ruleset.clone()) {
                    continue;
                }
                if let Some(s) = self.get(&b.ruleset, b.version)? {
                    out.push(s);
                }
            }
        }
        Ok(out)
    }
}

fn scope_key(s: &RuleScope) -> String {
    match s {
        RuleScope::Project(id) => format!("project:{id}"),
        RuleScope::Agent(id) => format!("agent:{id}"),
        RuleScope::Task(id) => format!("task:{id}"),
    }
}

/// The rules chosen for one task.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RuleSelection {
    /// The rules given to the model, in the order shown.
    pub applied: Vec<RuleRef>,
    /// Bound rules left out (reachable with `rules.lookup`).
    pub held_back: usize,
    /// The block the model sees. Empty when nothing applies.
    pub text: String,
}

impl RuleSelection {
    pub fn is_empty(&self) -> bool {
        self.applied.is_empty() && self.held_back == 0
    }
}

/// The default cap on the rules block, in estimated tokens.
pub const RULES_BUDGET: u32 = 400;

/// Choose the rules of `sets` that `task` needs, within `budget` tokens. Deterministic: the
/// same sets and task give the same selection.
pub fn select(sets: &[RuleSet], task: &str, budget: u32) -> RuleSelection {
    let words = terms(task);
    let mut scored: Vec<(u32, usize, &RuleSet, &Rule)> = Vec::new();
    let mut order = 0usize;
    let mut total = 0usize;
    for set in sets {
        for r in &set.rules {
            total += 1;
            let s = score(r, &words);
            if r.always || s > 0 {
                scored.push((if r.always { u32::MAX } else { s }, order, set, r));
            }
            order += 1;
        }
    }
    // Always-rules first, then by relevance, then in the order they were written.
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let mut sel = RuleSelection::default();
    let mut body = String::new();
    let mut used = 0u32;
    for (_, _, set, r) in scored {
        let line = format!("- [{}] {}\n", r.id, r.text);
        let cost = estimate_tokens(&line);
        if used + cost > budget && !r.always {
            continue;
        }
        used += cost;
        body.push_str(&line);
        sel.applied.push(RuleRef {
            set: set.name.clone(),
            version: set.version,
            id: r.id.clone(),
        });
    }
    sel.held_back = total - sel.applied.len();
    if sel.applied.is_empty() && sel.held_back == 0 {
        return sel;
    }
    let names: Vec<String> = sets
        .iter()
        .map(|s| format!("{} v{}", s.name, s.version))
        .collect();
    let _ = writeln!(sel.text, "Follow these rules ({}):", names.join(", "));
    sel.text.push_str(&body);
    if sel.applied.is_empty() {
        let _ = writeln!(sel.text, "(none matched this task)");
    }
    if sel.held_back > 0 {
        let _ = write!(
            sel.text,
            "{} more rules cover other kinds of work. Only if this task needs one that is not listed, rules.lookup(query) finds it.",
            sel.held_back
        );
    }
    sel.text = sel.text.trim_end().to_string();
    sel
}

const STOP: &[&str] = &[
    "the", "and", "for", "with", "that", "this", "from", "into", "are", "was", "were", "has",
    "have", "not", "but", "you", "your", "its", "all", "any", "can", "will", "should", "must",
    "every", "each", "what", "when", "how", "why", "who", "use", "using", "make", "about",
];

fn terms(text: &str) -> HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 3)
        .map(|w| w.to_lowercase())
        .filter(|w| !STOP.contains(&w.as_str()))
        .map(|w| stem(&w))
        .collect()
}

/// A crude stem so "tests" matches "testing" and "test".
fn stem(w: &str) -> String {
    for suffix in ["ing", "ies", "es", "ed", "s"] {
        if let Some(s) = w.strip_suffix(suffix) {
            if s.chars().count() >= 3 {
                return s.to_string();
            }
        }
    }
    w.to_string()
}

/// A tag match counts three, a word in the rule's text one.
fn score(r: &Rule, task: &HashSet<String>) -> u32 {
    let tags: HashSet<String> = r.tags.iter().map(|t| stem(&t.to_lowercase())).collect();
    let tag_hits = tags.iter().filter(|t| task.contains(*t)).count() as u32;
    let text_hits = terms(&r.text).intersection(task).count() as u32;
    tag_hits * 3 + text_hits
}

/// The `rules` tool: look up bound rules that were not in the task's block.
pub struct RulesTool {
    sets: Vec<RuleSet>,
}

pub const LOOKUP: &str = "rules.lookup";
pub const GET: &str = "rules.get";

impl RulesTool {
    pub fn new(sets: Vec<RuleSet>) -> Self {
        RulesTool { sets }
    }
}

#[async_trait::async_trait]
impl Adapter for RulesTool {
    fn name(&self) -> &str {
        "rules"
    }

    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![
            ActionDescriptor::new(
                LOOKUP,
                "find the bound rules (standards) that apply to a topic",
                RiskTier::Read,
            )
            .reusable()
            .with_input_schema(json!({
                "type": "object", "required": ["query"],
                "properties": { "query": { "type": "string" }, "limit": { "type": "integer" } }
            })),
            ActionDescriptor::new(GET, "read one bound rule by its id", RiskTier::Read)
                .reusable()
                .with_input_schema(json!({
                    "type": "object", "required": ["id"],
                    "properties": { "id": { "type": "string" } }
                })),
        ]
    }

    async fn run(
        &self,
        action: &str,
        input: Value,
        _ctx: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        let arg = |k: &str| input.get(k).and_then(Value::as_str).unwrap_or("").trim();
        let row = |s: &RuleSet, r: &Rule| json!({ "set": s.name, "version": s.version, "id": r.id, "text": r.text, "tags": r.tags });
        match action {
            LOOKUP => {
                let limit = input.get("limit").and_then(Value::as_u64).unwrap_or(8) as usize;
                let q = terms(arg("query"));
                let mut hits: Vec<(u32, Value)> = Vec::new();
                for s in &self.sets {
                    for r in &s.rules {
                        let sc = score(r, &q);
                        if sc > 0 {
                            hits.push((sc, row(s, r)));
                        }
                    }
                }
                hits.sort_by_key(|h| std::cmp::Reverse(h.0));
                let rules: Vec<Value> = hits.into_iter().take(limit).map(|(_, v)| v).collect();
                Ok(ActionOutput::json(
                    json!({ "query": arg("query"), "count": rules.len(), "rules": rules }),
                ))
            }
            GET => {
                let id = arg("id");
                for s in &self.sets {
                    if let Some(r) = s.get(id) {
                        return Ok(ActionOutput::json(row(s, r)));
                    }
                }
                Err(AdapterError::bad_input(
                    GET,
                    format!("no bound rule `{id}`"),
                ))
            }
            other => Err(AdapterError::UnknownAction(other.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_core::RuleSet;

    fn eng() -> RuleSet {
        let mut s = RuleSet::parse(
            "engineering",
            "- [E1] (always) Never commit a secret.\n\
             - [E2] Every public function has a test. #testing\n\
             - [E3] Database migrations are reversible. #database #migration\n\
             - [E4] Log errors with their context. #logging\n",
        );
        s.version = 2;
        s
    }

    #[test]
    fn selects_always_rules_and_relevant_ones_only() {
        let sel = select(&[eng()], "Add tests for the payment module", RULES_BUDGET);
        let ids: Vec<&str> = sel.applied.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["E1", "E2"]);
        assert_eq!(sel.held_back, 2);
        assert!(sel.text.contains("engineering v2"));
        assert!(sel.text.contains("[E2] Every public function has a test."));
        assert!(!sel.text.contains("migrations"));
        assert!(sel.text.contains("rules.lookup"));
        assert_eq!(sel.applied[0].to_string(), "engineering@2:E1");
    }

    #[test]
    fn the_budget_caps_relevant_rules_but_not_always_rules() {
        let sel = select(&[eng()], "testing database migration logging", 12);
        assert_eq!(sel.applied[0].id, "E1", "always rules are kept");
        assert!(sel.applied.len() < 4);
    }

    #[test]
    fn no_bound_rules_means_no_block() {
        let sel = select(&[], "anything", RULES_BUDGET);
        assert!(sel.is_empty());
        assert!(sel.text.is_empty());
    }

    #[test]
    fn rule_book_versions_and_binds() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let book = RuleBook::new(&store);
        let v1 = book.put(&RuleSet::parse("eng", "- a rule")).unwrap();
        assert_eq!(v1.version, 1);
        assert_eq!(
            book.put(&RuleSet::parse("eng", "- a rule"))
                .unwrap()
                .version,
            1
        );
        let v2 = book
            .put(&RuleSet::parse("eng", "- a rule\n- another"))
            .unwrap();
        assert_eq!(v2.version, 2);
        assert_eq!(book.get("eng", Some(1)).unwrap().unwrap().rules.len(), 1);
        assert_eq!(book.list().unwrap().len(), 1);

        book.bind(&RuleBinding::new("eng", RuleScope::Project("p".into())))
            .unwrap();
        book.bind(&RuleBinding::new("eng", RuleScope::Task("t".into())).pinned(1))
            .unwrap();
        let task_first = book
            .resolve(&[RuleScope::Task("t".into()), RuleScope::Project("p".into())])
            .unwrap();
        assert_eq!(task_first.len(), 1);
        assert_eq!(task_first[0].version, 1, "the task's pin wins");
        let project = book.resolve(&[RuleScope::Project("p".into())]).unwrap();
        assert_eq!(project[0].version, 2, "unpinned follows the newest");
        assert!(book.unbind("eng", &RuleScope::Task("t".into())).unwrap());
    }

    #[tokio::test]
    async fn the_tool_finds_held_back_rules() {
        struct NoBlobs;
        impl ark_tools::BlobResolver for NoBlobs {
            fn path(&self, _: &ark_core::ArtifactId) -> Option<std::path::PathBuf> {
                None
            }
            fn read(&self, id: &ark_core::ArtifactId) -> ark_tools::Result<Vec<u8>> {
                Err(AdapterError::ArtifactNotFound(id.to_string()))
            }
        }
        let tool = RulesTool::new(vec![eng()]);
        let blobs = NoBlobs;
        let ctx = RunCtx::new(&blobs);
        let out = tool
            .run(LOOKUP, json!({"query": "how should migrations work"}), &ctx)
            .await
            .unwrap();
        assert_eq!(out.output["rules"][0]["id"], "E3");
        let one = tool.run(GET, json!({"id": "e4"}), &ctx).await.unwrap();
        assert_eq!(one.output["version"], 2);
    }
}
