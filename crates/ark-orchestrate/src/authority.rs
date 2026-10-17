//! What an event run may do (ADR 0062): an [`AutomationAuthority`], approved with its
//! automation and copied unchanged onto every run. It is default-deny and data, not code: each
//! allowed action names every input it may get and how each is bound. The runtime checks it
//! before every call, reads included, and before the ordinary approval gate; no grant,
//! `allow_mutate` or model output widens it.
//!
//! | Bound | The input must be |
//! |---|---|
//! | `literal` | exactly this value |
//! | `event` | the matched event's `actor`, `subject` or `fields.<name>`, from the saved envelope |
//! | `path` | one of the authority's `paths`, resolved inside the project folder (no `..`, symlink or hard-link alias) |
//! | `free` | anything (a reply's text) |

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ark_core::RiskTier;
use ark_runtime::Verdict;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::automation::StoredEvent;

/// How one input of an allowed action is bound.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "bound", rename_all = "snake_case")]
pub enum Bound {
    Literal {
        value: Value,
    },
    /// `actor`, `subject` or `fields.<name>` of the event the run is for.
    Event {
        field: String,
    },
    Path,
    Free,
}

/// Whether a change within the authority asks the person or runs on its own.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// The person approves each one (the default).
    #[default]
    Ask,
    /// Runs without asking, counted against its limits before it runs.
    Unattended,
}

/// At most `count` calls in each window of `window_ms`, across the automation's runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Quota {
    pub count: u32,
    pub window_ms: u64,
}

/// One action a run may call, with every input it may get.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Allowed {
    pub action: String,
    #[serde(default)]
    pub inputs: BTreeMap<String, Bound>,
    #[serde(default)]
    pub mode: Mode,
    /// Calls of a change per run; a reply is sent once.
    #[serde(default = "one")]
    pub per_run: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quota: Option<Quota>,
}

fn one() -> u32 {
    1
}

impl Allowed {
    pub fn new(action: impl Into<String>) -> Self {
        Allowed {
            action: action.into(),
            inputs: BTreeMap::new(),
            mode: Mode::Ask,
            per_run: 1,
            quota: None,
        }
    }
    pub fn input(mut self, name: impl Into<String>, bound: Bound) -> Self {
        self.inputs.insert(name.into(), bound);
        self
    }
    pub fn unattended(mut self) -> Self {
        self.mode = Mode::Unattended;
        self
    }
    pub fn quota(mut self, count: u32, window_ms: u64) -> Self {
        self.quota = Some(Quota { count, window_ms });
        self
    }
}

/// Everything an event run may do. Nothing else runs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutomationAuthority {
    #[serde(default)]
    pub actions: Vec<Allowed>,
    /// Project files a `path` input may name, relative to the project folder.
    #[serde(default)]
    pub paths: Vec<String>,
}

/// The runtime's own tools an event run keeps: loading the allowed actions and reading back
/// its own results (each run works in a project of its own, so these see nothing else).
const META_ALLOWED: &[&str] = &[
    ark_runtime::catalog::LIST_ACTIONS,
    ark_runtime::catalog::GET_EVIDENCE,
    ark_runtime::catalog::RECORD_STATE,
    ark_runtime::catalog::RECORD_NOTE,
];

impl AutomationAuthority {
    pub fn allow(mut self, a: Allowed) -> Self {
        self.actions.push(a);
        self
    }
    pub fn read(mut self, path: impl Into<String>) -> Self {
        self.paths.push(path.into());
        self
    }

    pub fn allowed(&self, action: &str) -> Option<&Allowed> {
        self.actions.iter().find(|a| a.action == action)
    }

    /// Check one call of a run for `event`, in the project folder `workspace`.
    pub fn check(
        &self,
        action: &str,
        input: &Value,
        risk: Option<RiskTier>,
        event: &StoredEvent,
        workspace: Option<&Path>,
    ) -> Verdict {
        let Some(risk) = risk else {
            return self.check_meta(action, input);
        };
        let Some(allowed) = self.allowed(action) else {
            return Verdict::Refuse(format!("`{action}` is not one of the actions it may use"));
        };
        let Some(fields) = input.as_object() else {
            return Verdict::Refuse(format!("`{action}` needs its inputs as an object"));
        };
        for name in fields.keys() {
            if !allowed.inputs.contains_key(name) {
                return Verdict::Refuse(format!("`{action}` may not be given `{name}`"));
            }
        }
        for (name, bound) in &allowed.inputs {
            let given = fields.get(name);
            if let Err(why) = self.bound(name, bound, given, event, workspace) {
                return Verdict::Refuse(format!("`{action}`: {why}"));
            }
        }
        match (risk, allowed.mode) {
            (RiskTier::Mutate, Mode::Ask) => Verdict::Ask,
            _ => Verdict::Allow,
        }
    }

    fn check_meta(&self, action: &str, input: &Value) -> Verdict {
        if action == ark_runtime::catalog::LOAD_ACTION {
            let name = input.get("name").and_then(Value::as_str).unwrap_or("");
            return match self.allowed(name.trim()) {
                Some(_) => Verdict::Allow,
                None => Verdict::Refuse(format!("`{name}` is not one of the actions it may use")),
            };
        }
        if !ark_runtime::catalog::is_meta_tool(action) {
            return Verdict::Refuse(format!("`{action}` is not one of the actions it may use"));
        }
        if META_ALLOWED.contains(&action) {
            Verdict::Allow
        } else {
            Verdict::Refuse(format!("`{action}` is not available to this run"))
        }
    }

    fn bound(
        &self,
        name: &str,
        bound: &Bound,
        given: Option<&Value>,
        event: &StoredEvent,
        workspace: Option<&Path>,
    ) -> Result<(), String> {
        match bound {
            Bound::Free => Ok(()),
            Bound::Literal { value } => match given {
                Some(v) if v == value => Ok(()),
                _ => Err(format!("`{name}` must be {value}")),
            },
            Bound::Event { field } => {
                let want = event_field(event, field)
                    .ok_or_else(|| format!("the event has no `{field}` to bind `{name}` to"))?;
                let got = given.and_then(Value::as_str).map(str::trim);
                let same = match field.as_str() {
                    "actor" => got.map(str::to_lowercase) == Some(want.trim().to_lowercase()),
                    _ => got == Some(want.as_str()),
                };
                if same {
                    Ok(())
                } else {
                    Err(format!("`{name}` must be the event's {field} ({want})"))
                }
            }
            Bound::Path => {
                let got = given
                    .and_then(Value::as_str)
                    .ok_or_else(|| format!("`{name}` must name a file"))?;
                let root = workspace.ok_or("the project has no folder")?;
                let resolved = resolve(root, got)?;
                let allowed = self
                    .paths
                    .iter()
                    .filter_map(|p| resolve(root, p).ok())
                    .any(|p| p == resolved);
                if allowed {
                    Ok(())
                } else {
                    Err(format!("`{got}` is not a file it may read"))
                }
            }
        }
    }
}

/// A trusted value of the saved event.
fn event_field(e: &StoredEvent, field: &str) -> Option<String> {
    match field {
        "actor" => e.event.actor.clone(),
        "subject" => e.event.subject.clone(),
        _ => field
            .strip_prefix("fields.")
            .and_then(|f| e.event.fields.get(f).cloned()),
    }
}

/// `path` inside `root`, with every link followed. Refused: outside the folder, missing, not a
/// file, or a file with another hard link (it may be the same file as one outside).
fn resolve(root: &Path, path: &str) -> Result<PathBuf, String> {
    let root = std::fs::canonicalize(root).map_err(|e| format!("the project folder: {e}"))?;
    let full = std::fs::canonicalize(root.join(path)).map_err(|_| format!("no file `{path}`"))?;
    if !full.starts_with(&root) {
        return Err(format!("`{path}` is outside the project folder"));
    }
    let meta = std::fs::metadata(&full).map_err(|e| format!("`{path}`: {e}"))?;
    if !meta.is_file() {
        return Err(format!("`{path}` is not a file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.nlink() > 1 {
            return Err(format!("`{path}` has another name on disk"));
        }
    }
    Ok(full)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_tools::events::{Direction, ExternalEvent};
    use serde_json::json;

    fn event() -> StoredEvent {
        StoredEvent {
            id: "e-1".into(),
            seq: 1,
            saved_ms: 0,
            payload_bytes: 0,
            event: ExternalEvent {
                source: "gmail".into(),
                account: "office@example.com".into(),
                key: "gmail:m1".into(),
                kind: "message".into(),
                direction: Direction::Inbound,
                actor: Some("lara@example.com".into()),
                subject: Some("th-1".into()),
                at_ms: 0,
                label: String::new(),
                fields: [("message_id".to_string(), "<m1@x>".to_string())].into(),
                payload: None,
            },
        }
    }

    fn reply() -> AutomationAuthority {
        AutomationAuthority::default()
            .allow(
                Allowed::new("mail.reply")
                    .input(
                        "to",
                        Bound::Event {
                            field: "actor".into(),
                        },
                    )
                    .input(
                        "thread",
                        Bound::Event {
                            field: "subject".into(),
                        },
                    )
                    .input(
                        "message_id",
                        Bound::Event {
                            field: "fields.message_id".into(),
                        },
                    )
                    .input("body", Bound::Free),
            )
            .allow(Allowed::new("files.read").input("path", Bound::Path))
            .read("budget.csv")
    }

    fn check(a: &AutomationAuthority, action: &str, input: Value, ws: &Path) -> Verdict {
        let risk = if action.ends_with(".reply") {
            RiskTier::Mutate
        } else {
            RiskTier::Observe
        };
        a.check(action, &input, Some(risk), &event(), Some(ws))
    }

    fn refused(v: Verdict) -> String {
        match v {
            Verdict::Refuse(why) => why,
            other => panic!("not refused: {other:?}"),
        }
    }

    #[test]
    fn a_reply_is_bound_to_the_events_sender_thread_and_message() {
        let dir = tempfile::tempdir().unwrap();
        let a = reply();
        let ok = json!({"to": "Lara@example.com", "thread": "th-1", "message_id": "<m1@x>", "body": "Totals attached."});
        assert_eq!(
            check(&a, "mail.reply", ok.clone(), dir.path()),
            Verdict::Ask
        );
        let mut other = ok.clone();
        other["to"] = json!("evil@example.com");
        assert!(refused(check(&a, "mail.reply", other, dir.path())).contains("event's actor"));
        let mut thread = ok.clone();
        thread["thread"] = json!("th-2");
        assert!(refused(check(&a, "mail.reply", thread, dir.path())).contains("subject"));
        let mut cc = ok.clone();
        cc["cc"] = json!("evil@example.com");
        assert!(refused(check(&a, "mail.reply", cc, dir.path())).contains("`cc`"));
        let mut missing = ok;
        missing.as_object_mut().unwrap().remove("to");
        assert!(check(&a, "mail.reply", missing, dir.path()) != Verdict::Ask);
        let unattended = AutomationAuthority::default().allow(
            Allowed::new("mail.reply")
                .input("body", Bound::Free)
                .unattended(),
        );
        assert_eq!(
            check(&unattended, "mail.reply", json!({"body": "x"}), dir.path()),
            Verdict::Allow
        );
    }

    #[test]
    fn anything_not_named_is_refused_reads_and_meta_tools_included() {
        let dir = tempfile::tempdir().unwrap();
        let a = reply();
        assert!(
            refused(check(&a, "web.search", json!({"q": "x"}), dir.path())).contains("not one of")
        );
        assert!(refused(check(&a, "code.run", json!({}), dir.path())).contains("not one of"));
        assert!(refused(check(&a, "process.start", json!({}), dir.path())).contains("not one of"));
        let meta =
            |name: &str, input: Value| a.check(name, &input, None, &event(), Some(dir.path()));
        assert_eq!(
            meta("load_action", json!({"name": "mail.reply"})),
            Verdict::Allow
        );
        assert!(matches!(
            meta("load_action", json!({"name": "web.search"})),
            Verdict::Refuse(_)
        ));
        assert_eq!(
            meta("get_evidence", json!({"invocation": "i"})),
            Verdict::Allow
        );
        for name in [
            "search_evidence",
            "search_knowledge",
            "acquire_capability",
            "propose_capability",
            "find_capability",
            "present",
        ] {
            assert!(
                matches!(meta(name, json!({})), Verdict::Refuse(_)),
                "{name}"
            );
        }
    }

    #[test]
    fn a_path_must_be_a_named_file_inside_the_folder_with_no_alias() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("budget.csv"), "q4,10").unwrap();
        std::fs::write(dir.path().join("salaries.csv"), "x").unwrap();
        std::fs::write(outside.path().join("secret.txt"), "s").unwrap();
        let a = reply();
        let read = |p: &str| check(&a, "files.read", json!({ "path": p }), dir.path());
        assert_eq!(read("budget.csv"), Verdict::Allow);
        assert_eq!(read("./budget.csv"), Verdict::Allow, "the same file");
        assert!(refused(read("salaries.csv")).contains("not a file it may read"));
        assert!(refused(read("../x/budget.csv")).contains("no file"));
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(
                outside.path().join("secret.txt"),
                dir.path().join("link.csv"),
            )
            .unwrap();
            let linked = a.clone().read("link.csv");
            let v = check(
                &linked,
                "files.read",
                json!({"path": "link.csv"}),
                dir.path(),
            );
            assert!(refused(v).contains("outside the project folder"));
            // A symlink inside pointing at the allowed file reads the allowed file.
            std::os::unix::fs::symlink(dir.path().join("budget.csv"), dir.path().join("b.csv"))
                .unwrap();
            assert_eq!(read("b.csv"), Verdict::Allow);
            std::fs::hard_link(
                dir.path().join("budget.csv"),
                outside.path().join("copy.csv"),
            )
            .unwrap();
            assert!(refused(read("budget.csv")).contains("another name"));
        }
    }

    #[test]
    fn a_literal_input_must_match_exactly() {
        let dir = tempfile::tempdir().unwrap();
        let a = AutomationAuthority::default().allow(
            Allowed::new("sheet.append")
                .input(
                    "sheet",
                    Bound::Literal {
                        value: json!("orders"),
                    },
                )
                .input("row", Bound::Free),
        );
        let v = |s: &str| {
            a.check(
                "sheet.append",
                &json!({"sheet": s, "row": [1]}),
                Some(RiskTier::Mutate),
                &event(),
                Some(dir.path()),
            )
        };
        assert_eq!(v("orders"), Verdict::Ask);
        assert!(matches!(v("orders "), Verdict::Refuse(_)));
    }
}
