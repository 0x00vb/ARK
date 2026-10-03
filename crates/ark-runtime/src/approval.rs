//! Interactive approvals. `Mutate` actions never run on their own; before this module the only
//! options were "denied" or a blanket `allow_mutate`. An [`ApprovalGate`] lets a front-end ask a
//! human at the moment the agent wants to act, and remember a "yes" for the rest of a session.
//!
//! The gate is the single permission system. Front-ends only *render* a request and return a
//! decision through [`Approver`]; they never decide policy themselves. Grants are keyed by an
//! approval *class*: the action name by default, or whatever the profile's classifier returns
//! (a shell tool might class `cargo test` separately from `rm`).

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use ark_core::RiskTier;
use serde::{Deserialize, Serialize};

/// What the agent wants to do, in a form a front-end can show.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApprovalRequest {
    /// The action name, e.g. `manuscript.save`.
    pub action: String,
    /// The action's one-line description from its descriptor.
    pub summary: String,
    pub risk: RiskTier,
    /// The exact input the action would run with.
    pub input: serde_json::Value,
    /// The grant key "allow for this session" would remember.
    pub class: String,
    /// A compact one-line rendering of the call, e.g. `manuscript.save(file="ch4.md", …)`.
    pub preview: String,
    /// The orchestrated agent asking, when the question does not come from the front-end's
    /// own conversation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// That agent's task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
}

/// A human's answer to an [`ApprovalRequest`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    /// Run this one call.
    AllowOnce,
    /// Run it, and stop asking for this approval class until the session ends.
    AllowSession,
    /// Do not run it.
    Deny,
}

impl ApprovalDecision {
    pub fn allows(&self) -> bool {
        !matches!(self, ApprovalDecision::Deny)
    }
}

/// Asks a human. Implemented by a front-end (a terminal menu, a GUI dialog, a CI policy).
#[async_trait::async_trait]
pub trait Approver: Send + Sync {
    async fn decide(&self, request: &ApprovalRequest) -> ApprovalDecision;
}

/// Maps a call to its approval class. Supplied by the profile; the default is the action name.
pub type ApprovalClassifier = Arc<dyn Fn(&str, &serde_json::Value) -> String + Send + Sync>;

/// The approval policy for one working session: who to ask, and what has been granted.
pub struct ApprovalGate {
    approver: Arc<dyn Approver>,
    grants: Mutex<BTreeSet<String>>,
}

impl ApprovalGate {
    pub fn new(approver: Arc<dyn Approver>) -> Self {
        ApprovalGate {
            approver,
            grants: Mutex::new(BTreeSet::new()),
        }
    }

    /// Whether a class was granted for the session.
    pub fn is_granted(&self, class: &str) -> bool {
        self.grants.lock().unwrap().contains(class)
    }

    /// The classes granted so far.
    pub fn grants(&self) -> Vec<String> {
        self.grants.lock().unwrap().iter().cloned().collect()
    }

    /// Forget every grant (a new session starts clean).
    pub fn revoke_all(&self) {
        self.grants.lock().unwrap().clear();
    }

    /// Resolve a request: a granted class passes without asking; otherwise the approver
    /// decides, and an `AllowSession` answer is remembered.
    pub async fn check(&self, request: &ApprovalRequest) -> ApprovalDecision {
        if self.is_granted(&request.class) {
            return ApprovalDecision::AllowSession;
        }
        let decision = self.approver.decide(request).await;
        if decision == ApprovalDecision::AllowSession {
            self.grants.lock().unwrap().insert(request.class.clone());
        }
        decision
    }
}

/// An approver that always answers the same way: `AllowOnce` for `--yes` style automation,
/// `Deny` for a locked-down headless run.
pub struct FixedApprover(pub ApprovalDecision);

#[async_trait::async_trait]
impl Approver for FixedApprover {
    async fn decide(&self, _request: &ApprovalRequest) -> ApprovalDecision {
        self.0
    }
}

/// A compact one-line rendering of a call for an approval prompt: string values are quoted and
/// clipped, long argument lists are elided.
pub fn preview_call(action: &str, input: &serde_json::Value) -> String {
    let args = match input.as_object() {
        Some(map) if !map.is_empty() => {
            map.iter()
                .take(4)
                .map(|(k, v)| {
                    let shown = match v {
                        serde_json::Value::String(s) => format!("\"{}\"", clip(s, 48)),
                        other => clip(&other.to_string(), 48),
                    };
                    format!("{k}={shown}")
                })
                .collect::<Vec<_>>()
                .join(", ")
                + if map.len() > 4 { ", …" } else { "" }
        }
        _ => String::new(),
    };
    format!("{action}({args})")
}

fn clip(s: &str, max: usize) -> String {
    let flat = s.replace('\n', "⏎");
    if flat.chars().count() <= max {
        flat
    } else {
        format!("{}…", flat.chars().take(max).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Counting(AtomicUsize, ApprovalDecision);
    #[async_trait::async_trait]
    impl Approver for Counting {
        async fn decide(&self, _r: &ApprovalRequest) -> ApprovalDecision {
            self.0.fetch_add(1, Ordering::SeqCst);
            self.1
        }
    }

    fn req(class: &str) -> ApprovalRequest {
        ApprovalRequest {
            action: "doc.save".into(),
            summary: "save".into(),
            risk: RiskTier::Mutate,
            input: serde_json::json!({}),
            class: class.into(),
            preview: "doc.save()".into(),
            agent: None,
            task: None,
        }
    }

    #[tokio::test]
    async fn session_grant_stops_further_prompts_for_that_class_only() {
        let approver = Arc::new(Counting(
            AtomicUsize::new(0),
            ApprovalDecision::AllowSession,
        ));
        let gate = ApprovalGate::new(approver.clone());
        assert_eq!(
            gate.check(&req("doc.save")).await,
            ApprovalDecision::AllowSession
        );
        assert_eq!(
            gate.check(&req("doc.save")).await,
            ApprovalDecision::AllowSession
        );
        assert_eq!(
            approver.0.load(Ordering::SeqCst),
            1,
            "asked once, then remembered"
        );
        gate.check(&req("doc.delete")).await;
        assert_eq!(
            approver.0.load(Ordering::SeqCst),
            2,
            "another class asks again"
        );
        gate.revoke_all();
        assert!(!gate.is_granted("doc.save"));
    }

    #[tokio::test]
    async fn allow_once_is_not_remembered() {
        let approver = Arc::new(Counting(AtomicUsize::new(0), ApprovalDecision::AllowOnce));
        let gate = ApprovalGate::new(approver.clone());
        gate.check(&req("x")).await;
        gate.check(&req("x")).await;
        assert_eq!(approver.0.load(Ordering::SeqCst), 2);
        assert!(gate.grants().is_empty());
    }

    #[test]
    fn preview_is_compact() {
        let p = preview_call(
            "fs.write",
            &serde_json::json!({ "path": "a.md", "text": "x".repeat(200) }),
        );
        assert!(p.starts_with("fs.write(path=\"a.md\""), "{p}");
        assert!(p.len() < 120, "{p}");
    }
}
