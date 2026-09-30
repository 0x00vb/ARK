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
}

impl Default for DistillerSet {
    fn default() -> Self {
        DistillerSet::with_common()
    }
}

impl DistillerSet {
    /// Empty — only the generic fallback applies.
    pub fn new() -> Self {
        DistillerSet { distillers: Vec::new(), common: false }
    }

    /// The default: the domain-neutral common distiller plus the generic fallback.
    pub fn with_common() -> Self {
        DistillerSet { distillers: Vec::new(), common: true }
    }

    /// Add a distiller (tried after earlier ones, before the common distiller and the fallback).
    pub fn push(&mut self, d: Distiller) {
        self.distillers.push(d);
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
        if self.common {
            if let Some(r) = distill_common(action, out) {
                return r;
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
            let parts: Vec<String> = keys.iter().map(|k| format!("{}:{}", k, canonical(&map[*k]))).collect();
            format!("{{{}}}", parts.join(","))
        }
        serde_json::Value::Array(a) => format!("[{}]", a.iter().map(canonical).collect::<Vec<_>>().join(",")),
        other => other.to_string(),
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
            format!("failed: {}", inv.error.clone().unwrap_or_else(|| "unknown error".into())),
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
    out.get(k).and_then(|v| v.as_array()).cloned().unwrap_or_default()
}

/// The universal shapes: file read/list/search, command execution, edits and diffs. These
/// are produced by any agent that operates on a workspace, whatever the domain.
fn distill_common(action: &str, out: &serde_json::Value) -> Option<(String, Vec<String>)> {
    let s = |k: &str| get_str(out, k);
    let n = |k: &str| get_u64(out, k);
    let arr = |k: &str| get_arr(out, k);
    let verb = action.split_once('.').map(|(_, v)| v).unwrap_or(action);

    Some(match verb {
        "read" => (format!("read {} ({} lines)", s("path").unwrap_or_default(), n("lines").unwrap_or(0)), vec![s("path").unwrap_or_default()]),
        "list" | "ls" => {
            let ids: Vec<String> = arr("entries").iter().filter_map(|v| v.as_str().map(str::to_string)).take(MAX_IDENTIFIERS).collect();
            (format!("{} entry(ies) under {}", ids.len(), s("path").unwrap_or_default()), ids)
        }
        "search" | "grep" => {
            let count = n("count").unwrap_or_else(|| arr("matches").len() as u64);
            let ids: Vec<String> = arr("matches")
                .iter()
                .filter_map(|m| {
                    let p = m.get("path").and_then(|v| v.as_str())?;
                    let l = m.get("line").and_then(|v| v.as_u64());
                    Some(match l { Some(l) => format!("{p}:{l}"), None => p.to_string() })
                })
                .take(MAX_IDENTIFIERS)
                .collect();
            (format!("{count} match(es) for {}", s("query").unwrap_or_default()), ids)
        }
        "write" | "edit" | "apply" => {
            let path = s("path").unwrap_or_default();
            let delta = match (n("added"), n("removed")) {
                (Some(a), Some(r)) => format!(" (+{a} -{r})"),
                _ => String::new(),
            };
            (format!("edited {path}{delta}"), vec![path])
        }
        "exec" | "run" => {
            let cmd = s("command").unwrap_or_default();
            let code = out.get("exit_code").and_then(|v| v.as_i64());
            let ok = code == Some(0);
            let tail = s("summary").or_else(|| s("stdout_tail")).unwrap_or_default();
            let verdict = match code {
                Some(0) => "ok".to_string(),
                Some(c) => format!("exit {c}"),
                None => "ran".to_string(),
            };
            let ids = if ok { vec![] } else { vec![format!("exit={}", code.map(|c| c.to_string()).unwrap_or_default())] };
            (format!("{} — {verdict}{}", short(&cmd, 60), if tail.is_empty() { String::new() } else { format!(": {}", short(&tail, 100)) }), ids)
        }
        "diff" => {
            let files = n("files").unwrap_or(0);
            let added = n("added").unwrap_or(0);
            let removed = n("removed").unwrap_or(0);
            (format!("diff: {files} file(s), +{added} -{removed}"), vec![])
        }
        _ => return None,
    })
}

/// Fallback: describe an unknown output by its counts and top-level scalar fields.
pub fn generic_summary(out: &serde_json::Value) -> (String, Vec<String>) {
    match out {
        serde_json::Value::Object(map) => {
            let mut parts = Vec::new();
            for (k, v) in map.iter().take(6) {
                match v {
                    serde_json::Value::Array(a) => parts.push(format!("{k}={} items", a.len())),
                    serde_json::Value::String(s) => parts.push(format!("{k}={}", short(s, 40))),
                    serde_json::Value::Number(_) | serde_json::Value::Bool(_) => parts.push(format!("{k}={v}")),
                    _ => {}
                }
            }
            (if parts.is_empty() { "output recorded".into() } else { parts.join(", ") }, Vec::new())
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
        || s.chars().next().map(|c| c.is_ascii_uppercase()).unwrap_or(false)
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
        let ok = distill(&inv("shell.run", json!({}), json!({"command":"cargo build","exit_code":0})), &set());
        assert!(ok.summary.contains("cargo build — ok"));
        assert!(ok.identifiers.is_empty());
        let bad = distill(&inv("shell.exec", json!({}), json!({"command":"cargo test","exit_code":1,"summary":"1 failed"})), &set());
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
    fn unknown_action_uses_generic_summary() {
        let card = distill(&inv("weird.thing", json!({}), json!({"count": 5, "items": [1,2,3], "name": "abc"})), &set());
        assert!(card.summary.contains("count=5"));
        assert!(card.summary.contains("items=3 items"));
    }

    #[test]
    fn custom_distiller_wins_over_common_and_fallback() {
        let custom: Distiller = Arc::new(|action, _out| {
            (action == "note.count").then(|| ("counted words".to_string(), vec!["1200".to_string()]))
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
        assert!(set.summarize_ok("fs.list", &files).0.contains("2 entry(ies)"));
    }
}
