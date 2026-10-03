//! Tool-result distillation: turn a raw [`Invocation`] into a compact [`EvidenceCard`]
//! the model can reason over, *without spending a model call*.
//!
//! The engine keeps the full structured output on the invocation (and big bodies as blob
//! artifacts); the runtime only ever puts cards in the prompt. Distillers are deterministic
//! and shape-specific — each recognizes a family of outputs and pulls the count and the
//! salient identifiers. Anything unrecognized falls back to a generic key/shape summary.
//!
//! ## Extension seam
//!
//! ARK ships one domain-neutral distiller ([`common_distiller`]) covering the shapes any
//! agent that touches files or runs commands produces (read/list/search, exec, edit, diff).
//! A domain adds its own families by pushing distillers onto a [`DistillerSet`] — a security
//! product adds a function-list/xref/decompilation distiller; a data agent adds a
//! query-result distiller. The generic fallback always applies last, so an unrecognized
//! tool still yields a usable card.

use std::sync::Arc;

use ark_core::{EvidenceCard, Invocation, InvocationStatus};

const MAX_IDENTIFIERS: usize = 12;

/// A shape-specific summarizer: given an action name and its output, either produce a
/// `(summary, identifiers)` for a shape it recognizes, or return `None` to defer to the
/// next distiller.
pub type Distiller =
    Arc<dyn Fn(&str, &serde_json::Value) -> Option<(String, Vec<String>)> + Send + Sync>;

/// An ordered set of distillers, tried in order; the first `Some` wins. The consumer's own
/// distillers always run before ARK's common one (a domain knows its `list` or `read` better
/// than a generic shape rule), and [`generic_summary`] is the final fallback.
#[derive(Clone)]
pub struct DistillerSet {
    distillers: Vec<Distiller>,
    common: bool,
    /// A result no distiller recognizes goes whole when its JSON is at most this many tokens
    /// (`ARK_WHOLE_RESULT_TOKENS`, default 0: always the generic summary).
    whole_below: u32,
}

fn whole_below_from_env() -> u32 {
    std::env::var("ARK_WHOLE_RESULT_TOKENS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

impl Default for DistillerSet {
    fn default() -> Self {
        DistillerSet::with_common()
    }
}

impl DistillerSet {
    /// Empty — only the generic fallback applies.
    pub fn new() -> Self {
        DistillerSet {
            distillers: Vec::new(),
            common: false,
            whole_below: whole_below_from_env(),
        }
    }

    /// The default: the domain-neutral common distiller plus the generic fallback.
    pub fn with_common() -> Self {
        DistillerSet {
            distillers: Vec::new(),
            common: true,
            whole_below: whole_below_from_env(),
        }
    }

    /// Add a distiller (tried after earlier ones, before the common distiller and the fallback).
    pub fn push(&mut self, d: Distiller) {
        self.distillers.push(d);
    }

    /// Pass a result no distiller recognizes whole when it is at most `tokens` tokens; a
    /// summary of a small result can cost a `get_evidence` round trip to read it anyway.
    pub fn whole_below(mut self, tokens: u32) -> Self {
        self.whole_below = tokens;
        self
    }

    /// Builder form of [`push`](DistillerSet::push).
    pub fn with(mut self, d: Distiller) -> Self {
        self.distillers.push(d);
        self
    }

    /// Distill a successful invocation's output into `(summary, identifiers)`.
    fn summarize_ok(&self, action: &str, out: &serde_json::Value) -> (String, Vec<String>) {
        for d in &self.distillers {
            if let Some(r) = d(action, out) {
                return r;
            }
        }
        if let Some(r) = distill_builtin(action, out) {
            return r;
        }
        if self.common {
            if let Some(r) = distill_common(action, out) {
                return r;
            }
        }
        if self.whole_below > 0 {
            let whole = out.to_string();
            if ark_model::estimate_tokens(&whole) <= self.whole_below {
                return (whole, Vec::new());
            }
        }
        generic_summary(out)
    }
}

/// A stable digest of a tool call `(action, input)`, used to detect and skip duplicate
/// invocations. Object keys are sorted so equivalent inputs collide.
pub fn call_digest(action: &str, input: &serde_json::Value) -> String {
    format!("{action}|{}", canonical(input))
}

fn canonical(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let parts: Vec<String> = keys
                .iter()
                .map(|k| format!("{}:{}", k, canonical(&map[*k])))
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        serde_json::Value::Array(a) => format!(
            "[{}]",
            a.iter().map(canonical).collect::<Vec<_>>().join(",")
        ),
        other => other.to_string(),
    }
}

/// Results of ARK's own optional tools (`rules.*`, `diagram.*`), whose text is the point: a
/// rule's wording, a diagram's source. Applied whatever the set, after the app's distillers.
fn distill_builtin(action: &str, out: &serde_json::Value) -> Option<(String, Vec<String>)> {
    let s = |k: &str| out.get(k).and_then(|v| v.as_str()).unwrap_or("");
    match action {
        "rules.lookup" => {
            let rules = out.get("rules")?.as_array()?;
            if rules.is_empty() {
                return Some((
                    format!("no bound rule matches \"{}\"", s("query")),
                    Vec::new(),
                ));
            }
            let mut lines = Vec::new();
            let mut ids = Vec::new();
            for r in rules {
                let id = r.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let text = r.get("text").and_then(|v| v.as_str()).unwrap_or("");
                lines.push(format!("[{id}] {text}"));
                ids.push(id.to_string());
            }
            Some((format!("{} rules: {}", rules.len(), lines.join(" | ")), ids))
        }
        "rules.get" => Some((format!("[{}] {}", s("id"), s("text")), vec![s("id").into()])),
        "diagram.create" => Some((
            format!(
                "stored {} diagram \"{}\" v{} ({} lines) as {}",
                s("kind"),
                s("title"),
                out.get("version").and_then(|v| v.as_u64()).unwrap_or(1),
                out.get("lines").and_then(|v| v.as_u64()).unwrap_or(0),
                s("stored")
            ),
            Vec::new(),
        )),
        "diagram.read" => Some((
            format!(
                "{} diagram \"{}\" v{}:\n{}",
                s("kind"),
                s("title"),
                out.get("version").and_then(|v| v.as_u64()).unwrap_or(1),
                short(s("source"), 4000)
            ),
            vec![s("artifact").into()],
        )),
        _ => None,
    }
}

/// Distill an invocation into its evidence card using the given distiller set. Pure and
/// deterministic.
pub fn distill(inv: &Invocation, set: &DistillerSet) -> EvidenceCard {
    let ok = matches!(inv.status, InvocationStatus::Ok);
    let (summary, identifiers) = if ok {
        set.summarize_ok(&inv.action, &inv.output)
    } else {
        (
            format!(
                "failed: {}",
                inv.error.clone().unwrap_or_else(|| "unknown error".into())
            ),
            Vec::new(),
        )
    };
    EvidenceCard::new(inv.id.clone(), &inv.action, ok, summary)
        .with_identifiers(identifiers)
        .with_produced(inv.produced.clone())
        .with_confidence(if ok { 1.0 } else { 0.3 })
        .with_call_digest(call_digest(&inv.action, &inv.input))
}

/// The domain-neutral common distiller as a [`Distiller`], for building a [`DistillerSet`].
pub fn common_distiller() -> Distiller {
    Arc::new(distill_common)
}

// ---- output field helpers (shared by all distillers) ----------------------
fn get_str(out: &serde_json::Value, k: &str) -> Option<String> {
    out.get(k).and_then(|v| v.as_str()).map(str::to_string)
}
fn get_u64(out: &serde_json::Value, k: &str) -> Option<u64> {
    out.get(k).and_then(|v| v.as_u64())
}
fn get_arr(out: &serde_json::Value, k: &str) -> Vec<serde_json::Value> {
    out.get(k)
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
}

/// The universal shapes: file read/list/search, command execution, edits and diffs. These
/// are produced by any agent that operates on a workspace, whatever the domain.
fn distill_common(action: &str, out: &serde_json::Value) -> Option<(String, Vec<String>)> {
    let s = |k: &str| get_str(out, k);
    let n = |k: &str| get_u64(out, k);
    let arr = |k: &str| get_arr(out, k);
    let verb = action.split_once('.').map(|(_, v)| v).unwrap_or(action);
    // A verb only names a shape when the fields of that shape are there: `notes.read` that
    // returns `{text}` is not a file read, and summarizing it as one loses its content.
    let has = |k: &str| out.get(k).is_some();

    Some(match verb {
        "read" if has("path") || has("lines") => (
            format!(
                "read {} ({} lines)",
                s("path").unwrap_or_default(),
                n("lines").unwrap_or(0)
            ),
            vec![s("path").unwrap_or_default()],
        ),
        "list" | "ls" if has("entries") => {
            let ids: Vec<String> = arr("entries")
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .take(MAX_IDENTIFIERS)
                .collect();
            (
                format!(
                    "{} entry(ies) under {}",
                    ids.len(),
                    s("path").unwrap_or_default()
                ),
                ids,
            )
        }
        "search" | "grep" if has("matches") || has("count") => {
            let count = n("count").unwrap_or_else(|| arr("matches").len() as u64);
            let ids: Vec<String> = arr("matches")
                .iter()
                .filter_map(|m| {
                    let p = m.get("path").and_then(|v| v.as_str())?;
                    let l = m.get("line").and_then(|v| v.as_u64());
                    Some(match l {
                        Some(l) => format!("{p}:{l}"),
                        None => p.to_string(),
                    })
                })
                .take(MAX_IDENTIFIERS)
                .collect();
            (
                format!("{count} match(es) for {}", s("query").unwrap_or_default()),
                ids,
            )
        }
        "write" | "edit" | "apply" if has("path") => {
            let path = s("path").unwrap_or_default();
            let delta = match (n("added"), n("removed")) {
                (Some(a), Some(r)) => format!(" (+{a} -{r})"),
                _ => String::new(),
            };
            (format!("edited {path}{delta}"), vec![path])
        }
        "exec" | "run" if has("command") || has("exit_code") => {
            let cmd = s("command").unwrap_or_default();
            let code = out.get("exit_code").and_then(|v| v.as_i64());
            let ok = code == Some(0);
            let tail = s("summary")
                .or_else(|| s("stdout_tail"))
                .unwrap_or_default();
            let verdict = match code {
                Some(0) => "ok".to_string(),
                Some(c) => format!("exit {c}"),
                None => "ran".to_string(),
            };
            let ids = if ok {
                vec![]
            } else {
                vec![format!(
                    "exit={}",
                    code.map(|c| c.to_string()).unwrap_or_default()
                )]
            };
            (
                format!(
                    "{} — {verdict}{}",
                    short(&cmd, 60),
                    if tail.is_empty() {
                        String::new()
                    } else {
                        format!(": {}", short(&tail, 100))
                    }
                ),
                ids,
            )
        }
        "diff" if has("files") || has("added") || has("removed") => {
            let files = n("files").unwrap_or(0);
            let added = n("added").unwrap_or(0);
            let removed = n("removed").unwrap_or(0);
            (
                format!("diff: {files} file(s), +{added} -{removed}"),
                vec![],
            )
        }
        _ => return None,
    })
}

/// The longest array (as compact JSON) the generic summary shows row by row; a longer one is
/// shown by its count. `ARK_INLINE_ROWS=0` always shows the count (the v0.6.0 behavior).
const INLINE_ROWS_CHARS: usize = 400;

fn inline_rows() -> bool {
    std::env::var("ARK_INLINE_ROWS").map_or(true, |v| v != "0")
}

/// Fallback: describe an unknown output by its counts and top-level scalar fields. A small
/// array is shown whole (a count hides the values a task may
/// need, and a small model does not always open the result to find them), and a summary that
/// counts an array says how to read its rows.
pub fn generic_summary(out: &serde_json::Value) -> (String, Vec<String>) {
    match out {
        serde_json::Value::Object(map) => {
            let mut parts = Vec::new();
            let inline = inline_rows();
            let mut collapsed = false;
            for (k, v) in map.iter().take(6) {
                match v {
                    serde_json::Value::Array(_)
                        if inline && v.to_string().len() <= INLINE_ROWS_CHARS =>
                    {
                        parts.push(format!("{k}={v}"))
                    }
                    serde_json::Value::Array(a) => {
                        parts.push(format!("{k}={} items", a.len()));
                        collapsed = true;
                    }
                    serde_json::Value::String(s) => parts.push(format!("{k}={}", short(s, 40))),
                    serde_json::Value::Number(_) | serde_json::Value::Bool(_) => {
                        parts.push(format!("{k}={v}"))
                    }
                    _ => {}
                }
            }
            let mut summary = if parts.is_empty() {
                "output recorded".to_string()
            } else {
                parts.join(", ")
            };
            if inline && collapsed {
                summary.push_str(
                    " (rows not shown: get_evidence on this result, with `match`, reads them)",
                );
            }
            (summary, Vec::new())
        }
        serde_json::Value::Null => ("output recorded".into(), Vec::new()),
        other => (short(&other.to_string(), 200), Vec::new()),
    }
}

/// A crude "is this string worth surfacing" filter for identifier extraction: paths,
/// symbol-looking tokens, and dotted keys, not boilerplate. Exposed for domain distillers.
pub fn looks_interesting(s: &str) -> bool {
    let s = s.trim();
    if s.len() < 4 || s.len() > 120 {
        return false;
    }
    s.contains('/')
        || s.contains('.')
        || s.contains('_')
        || s.chars()
            .next()
            .map(|c| c.is_ascii_uppercase())
            .unwrap_or(false)
}

/// Truncate a string to `max` chars with an ellipsis. Exposed for domain distillers.
pub fn short(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max).collect();
        format!("{t}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_core::RiskTier;
    use serde_json::json;

    fn set() -> DistillerSet {
        DistillerSet::with_common()
    }

    fn inv(action: &str, input: serde_json::Value, output: serde_json::Value) -> Invocation {
        let mut i = Invocation::new(action, RiskTier::Read, input);
        i.mark_ok(output);
        i
    }

    #[test]
    fn call_digest_is_order_independent() {
        let a = call_digest("code.read", &json!({"artifact": "x", "at": "main"}));
        let b = call_digest("code.read", &json!({"at": "main", "artifact": "x"}));
        assert_eq!(a, b, "key order must not change the digest");
        let c = call_digest("code.read", &json!({"artifact": "y", "at": "main"}));
        assert_ne!(a, c);
    }

    #[test]
    fn distills_search_matches_as_file_line_pivots() {
        let out = json!({ "query": "cache", "count": 2, "matches": [
            {"path": "src/main.rs", "line": 42}, {"path": "src/cache.rs", "line": 7}
        ]});
        let card = distill(&inv("fs.search", json!({"query":"cache"}), out), &set());
        assert_eq!(card.summary, "2 match(es) for cache");
        assert!(card.identifiers.contains(&"src/main.rs:42".to_string()));
    }

    #[test]
    fn distills_command_run_verdict() {
        let ok = distill(
            &inv(
                "shell.run",
                json!({}),
                json!({"command":"cargo build","exit_code":0}),
            ),
            &set(),
        );
        assert!(ok.summary.contains("cargo build — ok"));
        assert!(ok.identifiers.is_empty());
        let bad = distill(
            &inv(
                "shell.exec",
                json!({}),
                json!({"command":"cargo test","exit_code":1,"summary":"1 failed"}),
            ),
            &set(),
        );
        assert!(bad.summary.contains("exit 1"));
        assert!(bad.summary.contains("1 failed"));
        assert!(bad.identifiers.contains(&"exit=1".to_string()));
    }

    #[test]
    fn distills_failure_with_low_confidence() {
        let mut i = Invocation::new("x.y", RiskTier::Read, json!({}));
        i.mark_failed("tool not found");
        let card = distill(&i, &set());
        assert!(!card.ok);
        assert!(card.summary.contains("tool not found"));
        assert!(card.confidence < 0.5);
    }

    #[test]
    fn a_verb_without_its_shape_falls_back_to_the_fields() {
        // `notes.read` returning text, `notes.write` returning a flag: not file operations.
        let read = distill(
            &inv("notes.read", json!({}), json!({"text": "the locket"})),
            &set(),
        );
        assert_eq!(read.summary, "text=the locket");
        let write = distill(
            &inv("notes.write", json!({}), json!({"saved": true, "chars": 9})),
            &set(),
        );
        assert!(write.summary.contains("saved=true"), "{}", write.summary);
        // With the fields of the shape, the shape wins.
        let file = distill(
            &inv("fs.read", json!({}), json!({"path": "a.md", "lines": 3})),
            &set(),
        );
        assert_eq!(file.summary, "read a.md (3 lines)");
    }

    #[test]
    fn a_small_unknown_result_can_go_whole() {
        // Too many rows to show inline, few enough tokens to pass whole at 2,000.
        let rows: Vec<_> = (0..40).map(|i| json!({ "id": i, "amount": 1.5 })).collect();
        let out = json!({ "orders": rows });
        let summary =
            |set: DistillerSet| distill(&inv("crm.orders", json!({}), out.clone()), &set).summary;
        assert!(summary(DistillerSet::with_common().whole_below(0)).starts_with("orders=40 items"));
        assert!(summary(DistillerSet::with_common().whole_below(2000)).contains("\"amount\":1.5"));
        // Larger than the limit: the summary again.
        assert!(summary(DistillerSet::with_common().whole_below(5)).starts_with("orders=40 items"));
    }

    #[test]
    fn unknown_action_uses_generic_summary() {
        let card = distill(
            &inv(
                "weird.thing",
                json!({}),
                json!({"count": 5, "items": [1,2,3], "name": "abc"}),
            ),
            &set(),
        );
        assert!(card.summary.contains("count=5"));
        // A small array is shown whole...
        assert!(card.summary.contains("items=[1,2,3]"), "{}", card.summary);
        // ...a large one by its count, with how to read its rows.
        let many: Vec<u32> = (0..500).collect();
        let card = distill(
            &inv("weird.thing", json!({}), json!({ "items": many })),
            &set(),
        );
        assert!(
            card.summary.starts_with("items=500 items"),
            "{}",
            card.summary
        );
        assert!(card.summary.contains("get_evidence"), "{}", card.summary);
    }

    #[test]
    fn custom_distiller_wins_over_common_and_fallback() {
        let custom: Distiller = Arc::new(|action, _out| {
            (action == "note.count")
                .then(|| ("counted words".to_string(), vec!["1200".to_string()]))
        });
        let set = DistillerSet::with_common().with(custom);
        let card = distill(&inv("note.count", json!({}), json!({"words": 1200})), &set);
        assert_eq!(card.summary, "counted words");
        assert_eq!(card.identifiers, vec!["1200"]);
    }

    #[test]
    fn consumer_distillers_win_over_the_common_one() {
        let mine: Distiller = Arc::new(|action: &str, _out: &serde_json::Value| {
            (action == "book.list").then(|| ("4 chapters".to_string(), vec![]))
        });
        let set = DistillerSet::with_common().with(mine);
        let out = serde_json::json!({ "chapters": [1, 2, 3, 4] });
        assert_eq!(set.summarize_ok("book.list", &out).0, "4 chapters");
        // Unclaimed shapes still reach the common distiller.
        let files = serde_json::json!({ "path": ".", "entries": ["a", "b"] });
        assert!(set
            .summarize_ok("fs.list", &files)
            .0
            .contains("2 entry(ies)"));
    }
}
