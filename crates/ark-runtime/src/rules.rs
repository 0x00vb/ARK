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
    /// Rules left out: bound rules reachable with `rules.lookup`, and sections of the project's
    /// own instructions the block names.
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

impl RuleSelection {
    /// The project's own instructions first, then the bound rules.
    pub fn with_project(mut self, p: ProjectInstructions) -> Self {
        self.text = if self.text.is_empty() {
            p.text
        } else {
            format!("{}\n\n{}", p.text, self.text)
        };
        let mut applied = p.applied;
        applied.append(&mut self.applied);
        self.applied = applied;
        self.held_back += p.held_back;
        self
    }
}

/// The project folder's own instructions for agents (ADR 0051): a file such as `AGENTS.md` at
/// the folder's root, in sections, and the subfolders that have their own. The application
/// names the files (`ArkBuilder::project_instructions`); the person's folder decides what they
/// say, so the block says whose they are.
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct ProjectInstructions {
    /// The block the model sees.
    pub text: String,
    /// The sections shown, as rules of a set named after the file (version 0: not stored).
    pub applied: Vec<RuleRef>,
    /// Sections not shown for the budget, named in the block.
    pub held_back: usize,
}

/// The default cap on the block, in estimated tokens.
pub const INSTRUCTIONS_BUDGET: u32 = 1_500;
/// Bytes read of an instructions file; a longer one is cut.
const INSTRUCTIONS_BYTES: u64 = 64 * 1024;
/// How deep subfolders are looked into for their own file, how many are named, and how many
/// folder entries the walk reads at most.
const NESTED_DEPTH: usize = 2;
const NESTED_LISTED: usize = 12;
const WALK_ENTRIES: usize = 4_000;
/// Folders of build output and installed packages: never a project's own instructions.
const NOT_WALKED: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "build",
    "vendor",
    "venv",
    "__pycache__",
];

/// The instructions of the folder `dir` for `task`: the first of `names` found at its root, as
/// many of its sections as fit `budget` tokens (the first always, cut to fit; then those that
/// share the most words with the task, shown in the file's order), the others named, and the
/// subfolders holding one of `names`. Everything, lists included, stays within about `budget`.
/// None when there is neither file. A name is a file name only (no folders); a link is not
/// followed.
pub fn project_instructions(
    dir: &std::path::Path,
    names: &[String],
    task: &str,
    budget: u32,
) -> Option<ProjectInstructions> {
    let names: Vec<&str> = names
        .iter()
        .map(|n| n.trim())
        .filter(|n| !n.is_empty() && !n.contains(['/', '\\']) && *n != "." && *n != "..")
        .collect();
    if names.is_empty() {
        return None;
    }
    let root = names
        .iter()
        .find_map(|n| read_plain_file(&dir.join(n)).map(|t| (n.to_string(), t)));
    let nested = nested_files(dir, &names);
    if root.is_none() && nested.is_empty() {
        return None;
    }
    let mut out = ProjectInstructions::default();
    let mut text = String::new();
    let nested_line = (!nested.is_empty()).then(|| {
        let file = root.as_ref().map(|(f, _)| f.as_str()).unwrap_or(names[0]);
        format!(
            "Folders with their own {file}: {}. Read the one for a folder before you change files in it.",
            nested.join(", ")
        )
    });
    if let Some((file, body)) = &root {
        let head = format!(
            "From the project's own file {file} (written for this project, not by ARK). Follow it where it applies to this task; the person's request comes first.\n"
        );
        text.push_str(&head);
        // What the lines around the sections cost comes off the sections' share.
        let reserve = estimate_tokens(&head)
            + nested_line.as_deref().map(estimate_tokens).unwrap_or(0)
            + HELD_LIST_TOKENS;
        let room = budget.saturating_sub(reserve).max(budget / 4);
        let all = sections(body);
        let words = terms(task);
        // The first section, then the others by how many of the task's words they share.
        let mut order: Vec<usize> = (1..all.len()).collect();
        order.sort_by_key(|&i| std::cmp::Reverse(terms(&all[i].1).intersection(&words).count()));
        order.insert(0, 0);
        let mut shown = vec![false; all.len()];
        let mut used = 0u32;
        for i in order {
            let cost = estimate_tokens(&all[i].1);
            if i == 0 || used + cost <= room {
                shown[i] = true;
                used += cost.min(room);
            }
        }
        let mut held: Vec<String> = Vec::new();
        for (i, (heading, section)) in all.into_iter().enumerate() {
            if !shown[i] {
                held.push(heading.chars().take(HEADING_CHARS).collect());
                continue;
            }
            let section: String = if estimate_tokens(&section) > room {
                section.chars().take(room as usize * 4).collect()
            } else {
                section
            };
            let _ = write!(text, "\n{}\n", section.trim_end());
            out.applied.push(RuleRef {
                set: file.clone(),
                version: 0,
                id: if heading.is_empty() {
                    "intro".to_string()
                } else {
                    heading.chars().take(HEADING_CHARS).collect()
                },
            });
        }
        if !held.is_empty() {
            out.held_back = held.len();
            let more = held.len().saturating_sub(HELD_LISTED);
            held.truncate(HELD_LISTED);
            let more = if more > 0 {
                format!(" and {more} more")
            } else {
                String::new()
            };
            let _ = write!(
                text,
                "\nNot shown here (read {file} for them): {}{more}.\n",
                held.join("; ")
            );
        }
    }
    if let Some(line) = nested_line {
        let _ = write!(text, "\n{line}");
    }
    out.text = text.trim().to_string();
    Some(out)
}

/// Section names kept, and how many sections left out are named.
const HEADING_CHARS: usize = 48;
const HELD_LISTED: usize = 12;
/// What the list of sections left out may cost: [`HELD_LISTED`] names of [`HEADING_CHARS`].
const HELD_LIST_TOKENS: u32 = 170;

/// A regular file's text (not a link), at most [`INSTRUCTIONS_BYTES`]; None when it is not one
/// or is empty.
fn read_plain_file(path: &std::path::Path) -> Option<String> {
    use std::io::Read as _;
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.file_type().is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(INSTRUCTIONS_BYTES)
        .read_to_end(&mut bytes)
        .ok()?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    (!text.trim().is_empty()).then_some(text)
}

/// Markdown sections: (heading text, the section with its heading). Text before the first
/// heading is a section with no heading. Lines in code fences are never headings.
fn sections(body: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut fenced = false;
    for line in body.lines() {
        let t = line.trim_start();
        if t.starts_with("```") || t.starts_with("~~~") {
            fenced = !fenced;
        }
        let level = t.chars().take_while(|c| *c == '#').count();
        let heading = !fenced && (1..=6).contains(&level) && t[level..].starts_with(' ');
        if heading || out.is_empty() {
            let name = if heading {
                t[level..].trim().to_string()
            } else {
                String::new()
            };
            out.push((name, String::new()));
        }
        let (_, s) = out.last_mut().expect("pushed above");
        s.push_str(line);
        s.push('\n');
    }
    out.retain(|(h, s)| !h.is_empty() || !s.trim().is_empty());
    out
}

/// Subfolders of `dir` (up to [`NESTED_DEPTH`] deep) holding one of `names`, as paths relative
/// to `dir`, sorted. Hidden folders, build output and links are not walked.
fn nested_files(dir: &std::path::Path, names: &[&str]) -> Vec<String> {
    let mut found = Vec::new();
    let mut seen = 0usize;
    let mut level: Vec<std::path::PathBuf> = vec![dir.to_path_buf()];
    for _ in 0..NESTED_DEPTH {
        let mut next = Vec::new();
        for folder in &level {
            if seen > WALK_ENTRIES {
                break;
            }
            let Ok(entries) = std::fs::read_dir(folder) else {
                continue;
            };
            for e in entries.flatten() {
                seen += 1;
                if seen > WALK_ENTRIES {
                    break;
                }
                let name = e.file_name().to_string_lossy().into_owned();
                let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                if !is_dir || name.starts_with('.') || NOT_WALKED.contains(&name.as_str()) {
                    continue;
                }
                let sub = e.path();
                let plain = |n: &&&str| {
                    std::fs::symlink_metadata(sub.join(n)).is_ok_and(|m| m.file_type().is_file())
                };
                if let Some(n) = names.iter().find(plain) {
                    if let Ok(rel) = sub.join(n).strip_prefix(dir) {
                        found.push(rel.to_string_lossy().replace('\\', "/"));
                    }
                }
                next.push(sub);
            }
        }
        level = next;
    }
    found.sort();
    found.truncate(NESTED_LISTED);
    found
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

    fn names() -> Vec<String> {
        vec!["AGENTS.md".into()]
    }

    const FILE: &str = "# House rules\n\nWrite in British English.\n\n## Chapters\n\nOne file per chapter, `chNN.md`.\n\n```\n# not a heading\n```\n\n## Before you finish\n\nRead the chapter aloud.\n";

    #[test]
    fn the_project_file_goes_in_whole_when_it_fits() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), FILE).unwrap();
        let p = project_instructions(dir.path(), &names(), "", INSTRUCTIONS_BUDGET).unwrap();
        assert!(p.text.starts_with(
            "From the project's own file AGENTS.md (written for this project, not by ARK)."
        ));
        assert!(
            p.text.contains("Write in British English.")
                && p.text.contains("Read the chapter aloud.")
        );
        let ids: Vec<&str> = p.applied.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(
            ids,
            ["House rules", "Chapters", "Before you finish"],
            "a fenced # is not a heading"
        );
        assert!(p.applied.iter().all(|r| r.set == "AGENTS.md"));
        assert_eq!(p.held_back, 0);
    }

    #[test]
    fn sections_over_the_budget_are_named_not_shown() {
        let dir = tempfile::tempdir().unwrap();
        let long = format!(
            "# Intro\n\nShort.\n\n## Style\n\n{}\n\n## Last\n\nEnd.\n",
            "word ".repeat(400)
        );
        std::fs::write(dir.path().join("AGENTS.md"), long).unwrap();
        let p = project_instructions(dir.path(), &names(), "", 300).unwrap();
        assert!(p.text.contains("Short."));
        assert!(!p.text.contains("word word"), "{}", p.text);
        assert!(
            p.text
                .contains("Not shown here (read AGENTS.md for them): Style."),
            "{}",
            p.text
        );
        assert_eq!(p.held_back, 1);
        // A first section over the budget is cut, never left out.
        let p = project_instructions(dir.path(), &names(), "", 10).unwrap();
        assert!(p.text.contains("# Intro"));
        assert!(estimate_tokens(&p.text) < 200);
    }

    #[test]
    fn the_sections_that_fit_are_the_ones_the_task_needs() {
        let dir = tempfile::tempdir().unwrap();
        let filler = "lorem ipsum dolor ".repeat(60);
        let file = format!(
            "# Book\n\nAbout this book.\n\n## Map\n\n{filler}\n\n## Covers\n\nCover images are 1600 by 2560 pixels. {filler}\n\n## Index\n\n{filler}\n"
        );
        std::fs::write(dir.path().join("AGENTS.md"), file).unwrap();
        let p = project_instructions(dir.path(), &names(), "make a new cover image", 500).unwrap();
        let ids: Vec<&str> = p.applied.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["Book", "Covers"], "{}", p.text);
        assert!(p
            .text
            .contains("Not shown here (read AGENTS.md for them): Map; Index."));
        assert!(
            estimate_tokens(&p.text) <= 500 + 40,
            "{}",
            estimate_tokens(&p.text)
        );
    }

    #[test]
    fn the_list_of_sections_left_out_is_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let mut file = String::from("# Intro\n\nShort.\n");
        for i in 0..200 {
            file.push_str(&format!(
                "\n## Section {i} {}\n\n{}\n",
                "long heading ".repeat(20),
                "text ".repeat(50)
            ));
        }
        std::fs::write(dir.path().join("AGENTS.md"), file).unwrap();
        let p = project_instructions(dir.path(), &names(), "", INSTRUCTIONS_BUDGET).unwrap();
        assert!(p.text.contains(" more."), "{}", p.text);
        assert!(
            estimate_tokens(&p.text) <= INSTRUCTIONS_BUDGET + 40,
            "{}",
            estimate_tokens(&p.text)
        );
    }

    #[test]
    fn subfolders_with_their_own_file_are_named() {
        let dir = tempfile::tempdir().unwrap();
        for d in [
            "app",
            "site/docs",
            ".hidden",
            "node_modules/pkg",
            "deep/a/b",
        ] {
            std::fs::create_dir_all(dir.path().join(d)).unwrap();
            std::fs::write(dir.path().join(d).join("AGENTS.md"), "# Here\n").unwrap();
        }
        let p = project_instructions(dir.path(), &names(), "", INSTRUCTIONS_BUDGET).unwrap();
        assert!(p.applied.is_empty(), "no file at the root: only the list");
        assert_eq!(
            p.text,
            "Folders with their own AGENTS.md: app/AGENTS.md, site/docs/AGENTS.md. Read the one for a folder before you change files in it."
        );
    }

    #[test]
    fn no_file_no_names_or_a_path_as_a_name_give_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            project_instructions(dir.path(), &names(), "", INSTRUCTIONS_BUDGET),
            None
        );
        std::fs::write(dir.path().join("AGENTS.md"), FILE).unwrap();
        assert_eq!(
            project_instructions(dir.path(), &[], "", INSTRUCTIONS_BUDGET),
            None
        );
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.md"), "# Secret\n").unwrap();
        let name = format!(
            "../{}/secret.md",
            outside.path().file_name().unwrap().to_string_lossy()
        );
        assert_eq!(
            project_instructions(dir.path(), &[name], "", INSTRUCTIONS_BUDGET),
            None
        );
        // Empty is no file.
        std::fs::write(dir.path().join("AGENTS.md"), "  \n").unwrap();
        assert_eq!(
            project_instructions(dir.path(), &names(), "", INSTRUCTIONS_BUDGET),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn links_are_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("AGENTS.md"), "# Elsewhere\n").unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("AGENTS.md"),
            dir.path().join("AGENTS.md"),
        )
        .unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("linked")).unwrap();
        assert_eq!(
            project_instructions(dir.path(), &names(), "", INSTRUCTIONS_BUDGET),
            None
        );
    }

    #[test]
    fn a_huge_file_is_cut_when_read() {
        let dir = tempfile::tempdir().unwrap();
        let huge = format!("# Big\n{}", "x".repeat(INSTRUCTIONS_BYTES as usize * 2));
        std::fs::write(dir.path().join("AGENTS.md"), huge).unwrap();
        let p = project_instructions(dir.path(), &names(), "", INSTRUCTIONS_BUDGET).unwrap();
        assert!(p.text.len() < INSTRUCTIONS_BUDGET as usize * 4 + 400);
    }

    #[test]
    fn project_instructions_come_before_bound_rules() {
        let p = ProjectInstructions {
            text: "From the project's own file AGENTS.md.".into(),
            applied: vec![RuleRef {
                set: "AGENTS.md".into(),
                version: 0,
                id: "Intro".into(),
            }],
            held_back: 1,
        };
        let sel = select(&[eng()], "add a database migration", 400).with_project(p);
        assert!(sel
            .text
            .starts_with("From the project's own file AGENTS.md.\n\nFollow these rules"));
        assert_eq!(sel.applied[0].id, "Intro");
        assert_eq!(sel.held_back, 1 + 2);
    }
}
