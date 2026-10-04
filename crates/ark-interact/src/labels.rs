//! Turning tool calls into words a person reads. The runtime reports `manuscript.read` with a
//! JSON input; a person wants "Reading chapter-04.md". A consumer labels its own actions;
//! everything else gets a readable default from the action name and its main argument.
//! Every front-end uses the same labels.

use std::sync::Arc;

use ark_sdk::prelude::ApprovalRequest;
use serde_json::Value;

use crate::fmt::clip;

/// Custom wording for a consumer's tools. Every method is optional; `None` falls back to the
/// default.
pub trait ToolLabeler: Send + Sync {
    /// The activity label for a call, e.g. `Reading chapter-04.md`.
    fn call(&self, _action: &str, _input: &Value) -> Option<String> {
        None
    }
    /// The one-line result under a finished call. `summary` is the runtime's distilled result.
    fn result(&self, _action: &str, _ok: bool, _summary: &str) -> Option<String> {
        None
    }
    /// The one-line call shown when asking for approval, e.g. `$ cargo test --workspace`.
    fn approval(&self, _request: &ApprovalRequest) -> Option<String> {
        None
    }
}

type LabelFn = Arc<dyn Fn(&Value) -> String + Send + Sync>;

/// Per-action labels, matched by exact action name or by `namespace.` prefix.
#[derive(Clone, Default)]
pub struct ActivityLabels {
    exact: Vec<(String, LabelFn)>,
    prefix: Vec<(String, LabelFn)>,
}

impl ActivityLabels {
    pub fn new() -> Self {
        Self::default()
    }

    /// Label one action: `.label("manuscript.read", |i| format!("Reading {}", i["file"]))`.
    pub fn label(
        mut self,
        action: impl Into<String>,
        f: impl Fn(&Value) -> String + Send + Sync + 'static,
    ) -> Self {
        self.exact.push((action.into(), Arc::new(f)));
        self
    }

    /// Label every action in a namespace: `.namespace("notes", |_| "Reading notes".into())`.
    pub fn namespace(
        mut self,
        ns: impl Into<String>,
        f: impl Fn(&Value) -> String + Send + Sync + 'static,
    ) -> Self {
        let mut ns = ns.into();
        if !ns.ends_with('.') {
            ns.push('.');
        }
        self.prefix.push((ns, Arc::new(f)));
        self
    }

    fn find(&self, action: &str) -> Option<&LabelFn> {
        self.exact
            .iter()
            .find(|(a, _)| a == action)
            .map(|(_, f)| f)
            .or_else(|| {
                self.prefix
                    .iter()
                    .find(|(p, _)| action.starts_with(p.as_str()))
                    .map(|(_, f)| f)
            })
    }
}

impl ToolLabeler for ActivityLabels {
    fn call(&self, action: &str, input: &Value) -> Option<String> {
        self.find(action).map(|f| f(input))
    }
}

/// The labeler chain an app uses: consumer labelers first, then the defaults.
#[derive(Clone, Default)]
pub struct Labels {
    labelers: Vec<Arc<dyn ToolLabeler>>,
}

impl Labels {
    pub fn new(labelers: Vec<Arc<dyn ToolLabeler>>) -> Self {
        Labels { labelers }
    }

    pub fn call_label(&self, action: &str, input: &Value) -> String {
        self.labelers
            .iter()
            .find_map(|r| r.call(action, input))
            .or_else(|| runtime_label(action, input))
            .unwrap_or_else(|| default_label(action, input))
    }

    pub fn result_detail(&self, action: &str, ok: bool, summary: &str) -> String {
        self.labelers
            .iter()
            .find_map(|r| r.result(action, ok, summary))
            .unwrap_or_else(|| summary.lines().next().unwrap_or("").to_string())
    }

    pub fn approval_line(&self, request: &ApprovalRequest) -> String {
        self.labelers
            .iter()
            .find_map(|r| r.approval(request))
            .or_else(|| runtime_label(&request.action, &request.input))
            .unwrap_or_else(|| request.preview.clone())
    }

    /// A retrieval meta-tool (`search_evidence "x"`, `get_evidence inv:…`) in words.
    pub fn retrieval_label(&self, what: &str) -> String {
        if what.starts_with("search_knowledge") {
            "Consulting background knowledge"
        } else if what.starts_with("search_evidence") {
            "Searching gathered evidence"
        } else {
            "Recalling evidence"
        }
        .to_string()
    }

    /// A capability meta-tool in words.
    pub fn capability_label(&self, action: &str) -> String {
        match action {
            "find_capability" => "Looking for a capability",
            "propose_capability" => "Proposing a capability",
            _ => "Acquiring a capability",
        }
        .to_string()
    }
}

/// Arguments that usually name the thing an action works on, in priority order. Only words
/// any domain uses; an application with its own (a process, a cell, a scene) labels its
/// actions with a `ToolLabeler`.
const SUBJECT_KEYS: &[&str] = &[
    "path", "file", "name", "query", "term", "pattern", "command", "cmd", "url", "host", "domain",
    "target", "at", "artifact", "id", "text",
];

const VERBS: &[(&[&str], &str)] = &[
    (&["read", "open", "cat", "view", "load"], "Reading"),
    (&["list", "ls", "entries", "tree"], "Listing"),
    (
        &["search", "find", "grep", "query", "lookup", "match"],
        "Searching",
    ),
    (&["save", "write", "put", "store", "append"], "Writing"),
    (
        &["edit", "update", "patch", "replace", "rewrite"],
        "Editing",
    ),
    (
        &["run", "exec", "execute", "shell", "test", "build"],
        "Running",
    ),
    (
        &["count", "wordcount", "measure", "stats", "size"],
        "Measuring",
    ),
    (
        &["fetch", "get", "http", "download", "request", "curl"],
        "Fetching",
    ),
    (
        &["info", "inspect", "describe", "show", "status", "health"],
        "Inspecting",
    ),
    (&["delete", "remove", "rm"], "Deleting"),
    (&["create", "new", "make", "init"], "Creating"),
    (&["trace", "watch", "monitor"], "Tracing"),
    (
        &["analyze", "analyse", "triage", "check", "scan"],
        "Analyzing",
    ),
    (&["compare", "diff"], "Comparing"),
    (&["summarize", "summary"], "Summarizing"),
    (&["extract"], "Extracting"),
];

/// An element number as given (`3` or `"[3]"`).
fn element(input: &Value) -> String {
    match input.get("ref") {
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::String(s)) => s.trim_matches(['[', ']']).to_string(),
        _ => "?".into(),
    }
}

/// When a schedule runs, in words, from the `agents.schedule` input.
fn when_words(input: &Value) -> String {
    let when: ark_orchestrate::When = serde_json::from_value(input.clone()).unwrap_or_default();
    match when.normalized() {
        Ok(w) => w.describe(),
        Err(_) => String::new(),
    }
}

/// A readable label from an action name and its input: `continuity.search {"term":"locket"}`
/// becomes `Searching "locket"`.
/// ARK's own tools that an app may offer (delegation, messages, diagrams), in words.
fn runtime_label(action: &str, input: &Value) -> Option<String> {
    let s = |k: &str| input.get(k).and_then(Value::as_str).unwrap_or("").trim();
    Some(match action {
        ark_orchestrate::DELEGATE => match s("objective") {
            "" => format!("Delegating to {}", s("to")),
            o => format!("Delegating to {}: {}", s("to"), clip(o, 60)),
        },
        ark_orchestrate::WAIT => "Waiting for delegated work".into(),
        ark_orchestrate::CREATE => format!("Creating agent {}", s("name")),
        ark_orchestrate::MESSAGE => format!("Messaging {}", s("to")),
        ark_orchestrate::SCHEDULE => format!(
            "Scheduling {} to {} {}",
            s("to"),
            clip(s("objective"), 60),
            when_words(input)
        )
        .trim_end()
        .to_string(),
        ark_orchestrate::UNSCHEDULE => format!("Stopping scheduled task {}", s("schedule")),
        "computer.apps" => "Listing the open apps".into(),
        "computer.look" => format!("Looking at {}", s("app")),
        "computer.open" => format!("Opening {}", s("app")),
        "computer.click" => format!("Clicking element {} in {}", element(input), s("app")),
        "computer.type" => format!(
            "Typing \"{}\" into element {} in {}{}",
            clip(s("text"), 60),
            element(input),
            s("app"),
            if input.get("submit").and_then(Value::as_bool) == Some(true) {
                ", then Return"
            } else {
                ""
            }
        ),
        "computer.keys" => format!("Pressing {} in {}", s("keys"), s("app")),
        "diagram.create" => match s("title") {
            "" => "Drawing a diagram".into(),
            t => format!("Drawing \"{}\"", clip(t, 60)),
        },
        "diagram.read" => "Reading a diagram".into(),
        _ => return None,
    })
}

pub fn default_label(action: &str, input: &Value) -> String {
    let (ns, verb) = action.split_once('.').unwrap_or((action, action));
    let verb_l = verb.to_ascii_lowercase();
    let gerund = VERBS.iter().find_map(|(words, g)| {
        words
            .iter()
            .any(|w| verb_l == *w || verb_l.starts_with(w) || (w.len() >= 4 && verb_l.ends_with(w)))
            .then_some(*g)
    });
    let subject = subject(input);
    match (gerund, subject) {
        (Some(g), Some((key, s))) => {
            if matches!(key, "query" | "term" | "pattern") {
                format!("{g} \"{s}\"")
            } else {
                format!("{g} {s}")
            }
        }
        (Some(g), None) => format!("{g} {}", ns.replace(['_', '-'], " ")),
        (None, Some((_, s))) => format!("{} · {s}", humanize(action)),
        (None, None) => humanize(action),
    }
}

fn subject(input: &Value) -> Option<(&'static str, String)> {
    let obj = input.as_object()?;
    for key in SUBJECT_KEYS {
        if let Some(v) = obj.get(*key) {
            let s = match v {
                Value::String(s) if !s.trim().is_empty() => s.clone(),
                Value::Number(n) => n.to_string(),
                _ => continue,
            };
            return Some((key, clip(&s, 60)));
        }
    }
    None
}

fn humanize(action: &str) -> String {
    let s = action.replace(['.', '_'], " ");
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn scheduling_and_the_computer_read_naturally() {
        let l = Labels::new(Vec::new());
        assert_eq!(
            l.call_label(
                "agents.schedule",
                &json!({ "to": "news", "objective": "summarize the news", "at": "8:30", "days": ["weekdays"] })
            ),
            "Scheduling news to summarize the news every weekday at 08:30"
        );
        assert_eq!(
            l.call_label(
                "computer.type",
                &json!({ "app": "Notes", "ref": 3, "text": "Plan", "submit": true })
            ),
            "Typing \"Plan\" into element 3 in Notes, then Return"
        );
        assert_eq!(
            l.call_label("computer.click", &json!({ "app": "Mail", "ref": "[2]" })),
            "Clicking element 2 in Mail"
        );
    }

    #[test]
    fn runtime_tools_read_naturally() {
        let l = Labels::new(Vec::new());
        assert_eq!(
            l.call_label(
                "agents.delegate",
                &json!({ "to": "reader-a", "objective": "read the spring review" })
            ),
            "Delegating to reader-a: read the spring review"
        );
        assert_eq!(
            l.call_label("agents.message", &json!({ "to": "lead", "text": "x" })),
            "Messaging lead"
        );
        assert_eq!(
            l.call_label("diagram.create", &json!({ "title": "Themes" })),
            "Drawing \"Themes\""
        );
    }

    #[test]
    fn default_labels_read_naturally() {
        assert_eq!(
            default_label("manuscript.read", &json!({ "file": "chapter-04.md" })),
            "Reading chapter-04.md"
        );
        assert_eq!(
            default_label("continuity.search", &json!({ "term": "locket" })),
            "Searching \"locket\""
        );
        assert_eq!(
            default_label("manuscript.list", &json!({})),
            "Listing manuscript"
        );
        assert_eq!(
            default_label("code.exec", &json!({ "command": "cargo test" })),
            "Running cargo test"
        );
        assert_eq!(
            default_label("manuscript.wordcount", &json!({ "file": "ch1.md" })),
            "Measuring ch1.md"
        );
        assert_eq!(
            default_label("calc.eval", &json!({ "expr": "6*7" })),
            "Calc eval"
        );
    }

    #[test]
    fn consumer_labels_win_over_defaults() {
        let labels = ActivityLabels::new()
            .label("manuscript.save", |i| {
                format!("Saving {}", i["file"].as_str().unwrap_or("?"))
            })
            .namespace("notes", |_| "Working in the notes".into());
        let a = Labels::new(vec![Arc::new(labels)]);
        assert_eq!(
            a.call_label("manuscript.save", &json!({ "file": "ch4.md" })),
            "Saving ch4.md"
        );
        assert_eq!(
            a.call_label("notes.tidy", &json!({})),
            "Working in the notes"
        );
        assert_eq!(
            a.call_label("manuscript.read", &json!({ "file": "x" })),
            "Reading x"
        );
    }
}
