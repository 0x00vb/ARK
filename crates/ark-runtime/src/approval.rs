//! Interactive approvals. `Mutate` actions never run on their own; before this module the only
//! options were "denied" or a blanket `allow_mutate`. An [`ApprovalGate`] lets a front-end ask a
//! human at the moment the agent wants to act, and remember a "yes" for the rest of a session.
//!
//! The gate is the single permission system. Front-ends only *render* a request and return a
//! decision through [`Approver`]; they never decide policy themselves. Grants are keyed by an
//! approval *class*: the action name by default, or whatever the profile's classifier returns
//! (a shell tool might class `cargo test` separately from `rm`).
//!
//! A "yes" lasts once, for the session, or always in a project ([`ApprovalDecision::AllowAlways`]).
//! A lasting grant is a record in the store, per project; the person lists and removes it
//! ([`lasting_grants`], [`remove_lasting_grant`]).

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use ark_core::{InvocationId, ProjectId, RiskTier};
use ark_store::Store;
use serde::{Deserialize, Serialize};

/// What the agent wants to do, in a form a front-end can show.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApprovalRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation: Option<InvocationId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectId>,
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
    /// The action asks at every call (`ActionDescriptor::ask_each_time`): no grant covers it,
    /// and an answer is never remembered.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ask_each_time: bool,
    /// The same call was under way when the app stopped, so it may already have happened. The
    /// person is asked whatever was granted, and the answer is not remembered.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cut_short: bool,
}

/// A human's answer to an [`ApprovalRequest`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    /// Run this one call.
    AllowOnce,
    /// Run it, and stop asking for this approval class until the session ends.
    AllowSession,
    /// Run it, and stop asking for this approval class in this project, across sessions and
    /// restarts, until the person removes the grant.
    AllowAlways,
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
    /// Where lasting grants are kept, and for which project.
    lasting: Mutex<Option<(Arc<Store>, String)>>,
}

/// The record kind of a lasting grant in the store.
pub const LASTING_GRANT: &str = "approval_grant";

/// A grant that lasts in a project until removed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LastingGrant {
    /// The approval class it covers (an action name, unless the profile classes calls).
    pub class: String,
    /// The action it was first given for, and how that call looked.
    pub action: String,
    pub preview: String,
    /// RFC 3339.
    pub granted_at: String,
}

fn grant_id(project: &str, class: &str) -> String {
    format!("{project}/{class}")
}

/// The lasting grants of a project, oldest first.
pub fn lasting_grants(store: &Store, project: &str) -> Vec<LastingGrant> {
    store
        .list_records(LASTING_GRANT, Some(project))
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| serde_json::from_value(v).ok())
        .collect()
}

/// Remove a lasting grant. Returns whether there was one.
pub fn remove_lasting_grant(store: &Store, project: &str, class: &str) -> bool {
    store
        .delete_record(LASTING_GRANT, &grant_id(project, class))
        .unwrap_or(false)
}

impl ApprovalGate {
    pub fn new(approver: Arc<dyn Approver>) -> Self {
        ApprovalGate {
            approver,
            grants: Mutex::new(BTreeSet::new()),
            lasting: Mutex::new(None),
        }
    }

    /// Keep and read lasting grants in `store` for `project` (the project a front-end has
    /// open). Without it, an "always" answer lasts for the session.
    pub fn keep_in(&self, store: Arc<Store>, project: &str) {
        *self.lasting.lock().unwrap() = Some((store, project.to_string()));
    }

    fn lasting_has(&self, class: &str) -> bool {
        match &*self.lasting.lock().unwrap() {
            Some((store, project)) => store
                .get_record(LASTING_GRANT, &grant_id(project, class))
                .ok()
                .flatten()
                .is_some(),
            None => false,
        }
    }

    /// Whether a class was granted for the session or, lasting, for the project.
    pub fn is_granted(&self, class: &str) -> bool {
        self.grants.lock().unwrap().contains(class) || self.lasting_has(class)
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
        if request.ask_each_time {
            // Only this call, whatever the answer said.
            return match self.approver.decide(request).await {
                ApprovalDecision::Deny => ApprovalDecision::Deny,
                _ => ApprovalDecision::AllowOnce,
            };
        }
        if self.grants.lock().unwrap().contains(&request.class) {
            return ApprovalDecision::AllowSession;
        }
        if self.lasting_has(&request.class) {
            return ApprovalDecision::AllowAlways;
        }
        let decision = self.approver.decide(request).await;
        self.remember(request, decision);
        decision
    }

    /// Keep what an answer grants: `AllowSession` for the session, `AllowAlways` in the
    /// project. For an answer that reached the asker some other way than [`ApprovalGate::check`]
    /// (a question kept across a restart and answered later).
    pub fn remember(&self, request: &ApprovalRequest, decision: ApprovalDecision) {
        if request.ask_each_time {
            return;
        }
        match decision {
            ApprovalDecision::AllowSession => {
                self.grants.lock().unwrap().insert(request.class.clone());
            }
            ApprovalDecision::AllowAlways => {
                // Kept only in the project's record, so removing it there asks again at once.
                // Without a store it lasts for the session.
                let lasting = self.lasting.lock().unwrap();
                if lasting.is_none() {
                    self.grants.lock().unwrap().insert(request.class.clone());
                }
                if let Some((store, project)) = &*lasting {
                    let grant = LastingGrant {
                        class: request.class.clone(),
                        action: request.action.clone(),
                        preview: request.preview.clone(),
                        granted_at: time::OffsetDateTime::now_utc()
                            .format(&time::format_description::well_known::Rfc3339)
                            .unwrap_or_default(),
                    };
                    let json = serde_json::to_value(&grant).expect("a grant serializes");
                    if let Err(e) = store.put_record(
                        LASTING_GRANT,
                        &grant_id(project, &request.class),
                        project,
                        &json,
                    ) {
                        tracing::warn!("could not keep the grant for {}: {e}", request.class);
                    }
                }
            }
            _ => {}
        }
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
            invocation: None,
            project: None,
            action: "doc.save".into(),
            summary: "save".into(),
            risk: RiskTier::Mutate,
            input: serde_json::json!({}),
            class: class.into(),
            preview: "doc.save()".into(),
            agent: None,
            task: None,
            ask_each_time: false,
            cut_short: false,
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
    async fn an_action_that_asks_each_time_is_never_covered_or_remembered() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()).unwrap());
        let approver = Arc::new(Counting(AtomicUsize::new(0), ApprovalDecision::AllowAlways));
        let gate = ApprovalGate::new(approver.clone());
        gate.keep_in(store.clone(), "p");
        // A grant for the class exists (the person allowed it before the rule, say).
        gate.check(&req("screen")).await;
        assert!(gate.is_granted("screen"));
        let mut each = req("screen");
        each.ask_each_time = true;
        assert_eq!(gate.check(&each).await, ApprovalDecision::AllowOnce);
        assert_eq!(gate.check(&each).await, ApprovalDecision::AllowOnce);
        assert_eq!(approver.0.load(Ordering::SeqCst), 3, "asked every time");
        let mut other = req("screen.other");
        other.ask_each_time = true;
        gate.check(&other).await;
        assert!(
            !gate.is_granted("screen.other") && lasting_grants(&store, "p").len() == 1,
            "an 'always' answer to it is not kept"
        );
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

    #[tokio::test]
    async fn an_always_grant_lasts_in_its_project_until_removed() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()).unwrap());
        let approver = Arc::new(Counting(AtomicUsize::new(0), ApprovalDecision::AllowAlways));
        let gate = ApprovalGate::new(approver.clone());
        gate.keep_in(store.clone(), "p1");
        gate.check(&req("doc.save")).await;

        // A new session (a new gate, as after a restart) in the same project does not ask.
        let later = ApprovalGate::new(approver.clone());
        later.keep_in(store.clone(), "p1");
        assert!(later.is_granted("doc.save"));
        assert_eq!(
            later.check(&req("doc.save")).await,
            ApprovalDecision::AllowAlways
        );
        assert_eq!(approver.0.load(Ordering::SeqCst), 1);

        // Another project asks.
        let other = ApprovalGate::new(approver.clone());
        other.keep_in(store.clone(), "p2");
        assert!(!other.is_granted("doc.save"));

        let kept = lasting_grants(&store, "p1");
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].class, "doc.save");
        assert_eq!(kept[0].preview, "doc.save()");
        assert!(remove_lasting_grant(&store, "p1", "doc.save"));
        let fresh = ApprovalGate::new(approver.clone());
        fresh.keep_in(store, "p1");
        assert!(!fresh.is_granted("doc.save"));
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
