//! Runtime events in a person's words. [`describe`] maps a [`HarnessEvent`] to an
//! [`Activity`]: what the agent is doing, labelled with the app's [`Labels`]. The terminal
//! prints it as a bullet line; the GUI draws a row. Events a person does not need (step
//! boundaries, context reports, model calls, text deltas) describe to `None`; the trace keeps
//! them.

use std::collections::HashMap;

use ark_sdk::prelude::{ApprovalDecision, FileBefore, HarnessEvent};
use serde::{Deserialize, Serialize};

use crate::labels::Labels;
use crate::report::Tone;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityKind {
    /// The agent drafted a plan (`items` holds the steps).
    Plan,
    /// A tool call started. `call` pairs it with its result.
    Tool,
    /// A tool call finished. `call` names the call it belongs to.
    Result,
    /// An identical earlier result was reused instead of running the tool.
    Reuse,
    /// Stored evidence or background knowledge was looked up.
    Retrieval,
    Capability,
    /// The agent asked the front-end to show something.
    Present,
    /// The working state changed.
    State,
    Note,
    /// An approval was answered.
    Approval,
    /// A message sent mid-turn reached the model.
    Steered,
    /// Something the person should notice (a refused action, a failed self-check).
    Warning,
    /// The turn was stopped.
    Stopped,
    /// Text the model wrote before calling tools. Live front-ends stream it as it arrives;
    /// only a replayed turn (see `Interaction::transcript`) carries it as an activity.
    Narration,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Activity {
    pub kind: ActivityKind,
    /// One line: `Reading chapter-04.md`, `3 matches in 2 files`, `allowed once`.
    pub label: String,
    /// A second line, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Sub-items (plan steps).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<String>,
    #[serde(default)]
    pub tone: Tone,
    /// The tool call id, for `Tool` and `Result`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call: Option<String>,
    /// The action name, for `Tool`, `Result` and `Reuse`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    /// Files of the project folder a `Result` wrote, for a front-end to offer to open.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    /// Files a `Result` changed or removed whose version before is kept, for a front-end to
    /// offer to put back (ADR 0034).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub before: Vec<FileBefore>,
}

impl Activity {
    pub(crate) fn new(kind: ActivityKind, label: impl Into<String>) -> Self {
        Activity {
            kind,
            label: label.into(),
            detail: None,
            items: Vec::new(),
            tone: Tone::Normal,
            call: None,
            action: None,
            files: Vec::new(),
            before: Vec::new(),
        }
    }
    fn detail(mut self, d: impl Into<String>) -> Self {
        let d = d.into();
        if !d.trim().is_empty() {
            self.detail = Some(d);
        }
        self
    }
    fn tone(mut self, t: Tone) -> Self {
        self.tone = t;
        self
    }
}

/// Describe one runtime event. `artifact_names` maps artifact ids to names so a call on an
/// artifact reads by name (ids may be prefixes).
pub fn describe(
    labels: &Labels,
    artifact_names: &HashMap<String, String>,
    ev: &HarnessEvent,
) -> Option<Activity> {
    use ActivityKind as K;
    Some(match ev {
        HarnessEvent::Classified { .. }
        | HarnessEvent::Step { .. }
        | HarnessEvent::Context { .. }
        | HarnessEvent::TextDelta { .. }
        | HarnessEvent::TextWithdrawn { .. }
        | HarnessEvent::ModelCall { .. }
        | HarnessEvent::ApprovalRequested { .. }
        | HarnessEvent::Answer { .. } => return None,
        HarnessEvent::Plan { steps } => {
            let mut a = Activity::new(K::Plan, "Planning");
            a.items = steps.iter().take(6).cloned().collect();
            a
        }
        HarnessEvent::ToolCall { id, action, input } => {
            let shown = with_artifact_names(input, artifact_names);
            let mut a = Activity::new(K::Tool, labels.call_label(action, &shown));
            a.call = Some(id.clone());
            a.action = Some(action.clone());
            a
        }
        HarnessEvent::ToolResult {
            id,
            action,
            ok,
            summary,
            error,
            files,
            before,
            ..
        } => {
            let mut a = if *ok {
                Activity::new(K::Result, labels.result_detail(action, true, summary))
                    .tone(Tone::Muted)
            } else {
                let msg = error.clone().unwrap_or_else(|| summary.clone());
                Activity::new(K::Result, msg.lines().next().unwrap_or("").to_string())
                    .tone(Tone::Error)
            };
            a.call = Some(id.clone());
            a.action = Some(action.clone());
            a.files = files.clone();
            a.before = before.clone();
            a
        }
        HarnessEvent::Duplicate { action, reused } => {
            let mut a = Activity::new(K::Reuse, format!("Reusing an earlier result · {action}"))
                .detail(reused.clone());
            a.action = Some(action.clone());
            a
        }
        HarnessEvent::Retrieval { what } => {
            Activity::new(K::Retrieval, labels.retrieval_label(what)).detail(what.clone())
        }
        HarnessEvent::NeedsApproval { action } => {
            Activity::new(K::Warning, format!("{action} needs approval; not run"))
                .tone(Tone::Warning)
        }
        HarnessEvent::ApprovalResolved {
            decision, class, ..
        } => match decision {
            ApprovalDecision::AllowOnce => {
                Activity::new(K::Approval, "allowed once").tone(Tone::Muted)
            }
            ApprovalDecision::AllowSession => {
                Activity::new(K::Approval, format!("allowed for this session ({class})"))
                    .tone(Tone::Muted)
            }
            ApprovalDecision::AllowAlways => {
                Activity::new(K::Approval, format!("allowed in this project ({class})"))
                    .tone(Tone::Muted)
            }
            ApprovalDecision::Deny => Activity::new(K::Approval, "denied").tone(Tone::Warning),
        },
        HarnessEvent::AwaitingConnection { display, .. } => {
            Activity::new(K::Warning, display.clone()).tone(Tone::Muted)
        }
        HarnessEvent::Capability { action, detail } => {
            Activity::new(K::Capability, labels.capability_label(action))
                .detail(detail.lines().next().unwrap_or("").to_string())
        }
        HarnessEvent::StateUpdate { kind, text } => {
            Activity::new(K::State, format!("noted {}", kind.replace('_', " ")))
                .detail(text.clone())
                .tone(Tone::Muted)
        }
        HarnessEvent::Note { kind, body } => Activity::new(K::Note, format!("note ({kind})"))
            .detail(body.clone())
            .tone(Tone::Muted),
        HarnessEvent::Present { directive } => {
            let h = directive.headline();
            Activity::new(
                K::Present,
                format!("Showing {}", h.strip_prefix("show ").unwrap_or(&h)),
            )
        }
        HarnessEvent::SelfCheck { passed, detail } => {
            if *passed {
                return None;
            }
            Activity::new(K::Warning, detail.clone()).tone(Tone::Warning)
        }
        HarnessEvent::Steered { text } => {
            Activity::new(K::Steered, "Your message reached the agent").detail(text.clone())
        }
        HarnessEvent::Retry { repair: true, .. } => {
            Activity::new(K::Warning, "Redoing its last step").tone(Tone::Muted)
        }
        HarnessEvent::Retry {
            attempt, delay_ms, ..
        } => Activity::new(
            K::Warning,
            format!(
                "Model call failed; retrying ({attempt}) in {:.1}s",
                *delay_ms as f64 / 1000.0
            ),
        )
        .tone(Tone::Warning),
        HarnessEvent::Stall {
            idle_steps,
            stopped,
        } => Activity::new(
            K::Warning,
            if *stopped {
                format!("No progress in {idle_steps} steps; answering with what is known")
            } else {
                format!("No progress in {idle_steps} steps; asked the agent to change course")
            },
        )
        .tone(Tone::Warning),
        HarnessEvent::Rules { applied, .. } if applied.is_empty() => return None,
        HarnessEvent::Rules { applied, held_back } => {
            let n = applied.len();
            // By where they come from: "AGENTS.md: Intro, Style; engineering: E1".
            let mut groups: Vec<(String, Vec<String>)> = Vec::new();
            for r in applied {
                match groups.last_mut() {
                    Some((set, ids)) if *set == r.set => ids.push(r.id.clone()),
                    _ => groups.push((r.set.clone(), vec![r.id.clone()])),
                }
            }
            let ids: Vec<String> = groups
                .into_iter()
                .map(|(set, ids)| format!("{set}: {}", ids.join(", ")))
                .collect();
            let more = if *held_back > 0 {
                format!(" ({held_back} more not shown)")
            } else {
                String::new()
            };
            Activity::new(
                K::Note,
                format!(
                    "Following {n} {}{more}",
                    if n == 1 { "rule" } else { "rules" }
                ),
            )
            .detail(ids.join("; "))
            .tone(Tone::Muted)
        }
        HarnessEvent::Cancelled => Activity::new(K::Stopped, STOPPED).tone(Tone::Warning),
        HarnessEvent::Interrupted => Activity::new(K::Stopped, INTERRUPTED).tone(Tone::Warning),
    })
}

/// A turn the person stopped.
pub const STOPPED: &str = "Stopped.";
/// A turn the app stopped (it quit, crashed, or the computer did) before it finished.
pub const INTERRUPTED: &str = "Interrupted before it finished.";
/// A call that was under way when its turn was interrupted.
pub const CUT_SHORT: &str = "cut short";
/// A call asked about that never ran because its turn was interrupted.
pub const NOT_RUN: &str = "not run";

/// The calls of a turn that did not finish, as its records say, put into its replayed
/// activity: a call whose result the trace never got shows done, failed, cut short, or not run
/// (it waited for an answer). With no trace at all, the calls alone are the activity.
/// `reported` holds the invocations the trace has a result for.
pub fn settle(
    activity: &mut Vec<Activity>,
    reported: &std::collections::HashSet<String>,
    calls: &[ark_sdk::prelude::ark_core::Invocation],
    labels: &Labels,
    artifact_names: &HashMap<String, String>,
) {
    use ark_sdk::prelude::ark_core::InvocationStatus as S;
    use ActivityKind as K;
    let mut open: Vec<&ark_sdk::prelude::ark_core::Invocation> = calls
        .iter()
        .filter(|c| !reported.contains(c.id.as_str()))
        .collect();
    let result =
        |call: Option<String>, action: &str, c: Option<&ark_sdk::prelude::ark_core::Invocation>| {
            let mut a = match c.map(|c| (&c.status, c.error.as_deref())) {
                Some((S::Ok, _)) => {
                    let summary = c.map(|c| c.output.to_string()).unwrap_or_default();
                    Activity::new(K::Result, labels.result_detail(action, true, &summary))
                        .tone(Tone::Muted)
                }
                Some((S::Failed, Some(e)))
                    if e != ark_sdk::prelude::ark_runtime::engine::INTERRUPTED_CALL =>
                {
                    Activity::new(K::Result, e.lines().next().unwrap_or("").to_string())
                        .tone(Tone::Error)
                }
                Some((S::PendingApproval, _)) | None => {
                    Activity::new(K::Result, NOT_RUN).tone(Tone::Warning)
                }
                _ => Activity::new(K::Result, CUT_SHORT).tone(Tone::Warning),
            };
            a.call = call;
            a.action = Some(action.to_string());
            a
        };
    let answered: std::collections::HashSet<String> = activity
        .iter()
        .filter(|a| a.kind == K::Result)
        .filter_map(|a| a.call.clone())
        .collect();
    let mut out = Vec::with_capacity(activity.len());
    for a in activity.drain(..) {
        let pending = a.kind == K::Tool && a.call.as_ref().is_some_and(|c| !answered.contains(c));
        let (call, action) = (a.call.clone(), a.action.clone().unwrap_or_default());
        out.push(a);
        if pending {
            let i = open.iter().position(|c| c.action == action);
            let c = i.map(|i| open.remove(i));
            out.push(result(call, &action, c));
        }
    }
    // Calls the trace never saw (it was not saved, or not yet): from their records alone.
    for (n, c) in open.into_iter().enumerate() {
        let id = format!("stored-{n}");
        let shown = with_artifact_names(&c.input, artifact_names);
        let mut a = Activity::new(K::Tool, labels.call_label(&c.action, &shown));
        a.call = Some(id.clone());
        a.action = Some(c.action.clone());
        out.push(a);
        out.push(result(Some(id), &c.action, Some(c)));
    }
    *activity = out;
}

/// Replace an `artifact` id argument with the artifact's name, for display.
pub fn with_artifact_names(
    input: &serde_json::Value,
    names: &HashMap<String, String>,
) -> serde_json::Value {
    let mut v = input.clone();
    if let Some(a) = v
        .get("artifact")
        .and_then(|a| a.as_str())
        .map(str::to_string)
    {
        let found = names.get(&a).or_else(|| {
            names
                .iter()
                .find(|(k, _)| a.len() >= 4 && k.starts_with(&a))
                .map(|(_, n)| n)
        });
        if let Some(name) = found {
            v["artifact"] = serde_json::Value::String(name.clone());
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rules_name_the_file_they_come_from() {
        let r = |set: &str, id: &str| {
            serde_json::from_value(json!({ "set": set, "version": 0, "id": id })).unwrap()
        };
        let a = describe(
            &Labels::default(),
            &HashMap::new(),
            &HarnessEvent::Rules {
                applied: vec![
                    r("AGENTS.md", "Intro"),
                    r("AGENTS.md", "Style"),
                    r("eng", "E1"),
                ],
                held_back: 2,
            },
        )
        .unwrap();
        assert_eq!(a.label, "Following 3 rules (2 more not shown)");
        assert_eq!(
            a.detail.as_deref(),
            Some("AGENTS.md: Intro, Style; eng: E1")
        );
    }

    #[test]
    fn calls_and_results_pair_by_id() {
        let labels = Labels::default();
        let names = HashMap::from([("abcdef123456".to_string(), "notes.md".to_string())]);
        let call = describe(
            &labels,
            &names,
            &HarnessEvent::ToolCall {
                id: "c1".into(),
                action: "notes.read".into(),
                input: json!({ "artifact": "abcdef" }),
            },
        )
        .unwrap();
        assert_eq!(call.label, "Reading notes.md");
        assert_eq!(call.call.as_deref(), Some("c1"));
        let failed = describe(
            &labels,
            &names,
            &HarnessEvent::ToolResult {
                id: "c1".into(),
                action: "notes.read".into(),
                ok: false,
                summary: "failed".into(),
                invocation: "i".into(),
                duration_ms: None,
                error: Some("no such file\nmore".into()),
                context_tokens: 0,
                raw_tokens: 0,
                files: Vec::new(),
                before: Vec::new(),
            },
        )
        .unwrap();
        assert_eq!(failed.label, "no such file");
        assert_eq!(failed.tone, Tone::Error);
        assert!(describe(&labels, &names, &HarnessEvent::Step { index: 1 }).is_none());
    }
}
