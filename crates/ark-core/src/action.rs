//! Actions: the typed operations adapters expose. Both the human and the agent invoke
//! tools only through these. An action *descriptor* is pure data (it lives in core); the
//! code that runs it lives in a tool adapter (`ark-tools`).

use crate::capability::Capability;
use serde::{Deserialize, Serialize};

/// The authority a person gives one chat for its next turn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    /// Inspect and answer. State-changing actions are not offered.
    Plan,
    /// Ask before state-changing actions that no existing grant covers.
    #[default]
    Manual,
    /// Run state-changing actions without asking, except those that ask at every call
    /// ([`ActionDescriptor::ask_each_time`]: spending money, the person's own screen, installing,
    /// standing work) and those only the person may decide ([`ActionDescriptor::person_only`]).
    /// With nobody to ask (no approval gate) it runs only [`ActionDescriptor::auto_eligible`].
    Auto,
}

/// How consequential an action is. Drives the approval gate: `Read` and `Observe`
/// auto-run; `Mutate` always needs explicit human approval before it executes.
///
/// The tiers are deliberately domain-neutral: the middle tier is "observes an external
/// system read-only", which covers reading a remote resource, listing processes, or a
/// read-only query of an authorized target — the same low-friction posture, whatever the
/// subject of study is. The top tier is "changes state somewhere outside the workspace".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskTier {
    /// Pure local analysis, no side effects: parse, search indexed evidence, read a file
    /// already in the workspace, transform data in memory.
    Read,
    /// Observes an external system read-only, changing nothing on it: fetch a URL, list
    /// processes, read remote metadata, a plain query of an authorized target. Auto-runs
    /// — observation stays frictionless.
    #[serde(alias = "device_read")]
    Observe,
    /// Changes state: write or modify a file, push to a remote system, send state-changing
    /// traffic, spend money. Needs approval unless a narrow action explicitly opts into Auto.
    Mutate,
}

impl RiskTier {
    /// Whether this tier runs without asking the human first.
    pub fn auto_runs(&self) -> bool {
        matches!(self, RiskTier::Read | RiskTier::Observe)
    }
}

/// Whether an earlier result of the same call may stand in for running it again. Only a
/// successful, completed result is ever reused; a failure, a timeout or a cancelled call runs
/// again.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reuse {
    /// The runtime decides. Within a turn an identical call is reused until an action that
    /// changes state runs (a write may change what a read returns). Across turns it is
    /// reused only when it is a `Read` of a stored artifact: those bytes cannot change.
    #[default]
    Auto,
    /// The output depends only on the input: reuse it from any earlier turn, even after
    /// state changes.
    Stable,
    /// Every call observes something new (a live capture, a clock, a counter): always run.
    Volatile,
}

/// A capability an adapter offers. The `name` is namespaced `adapter.verb`
/// (e.g. `fs.read`, `web.get`, `draft.expand`). The schemas are JSON Schema documents;
/// they are what the agent reads to call the tool, and what a UI can render as a form.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ActionDescriptor {
    pub name: String,
    pub summary: String,
    pub risk: RiskTier,
    /// JSON Schema for the input object.
    pub input_schema: serde_json::Value,
    /// JSON Schema for the output object (best-effort; may be permissive).
    #[serde(default)]
    pub output_schema: serde_json::Value,
    /// The capability categories this action serves (what kind of knowledge it produces).
    /// Optional: an adapter may leave it empty and let the runtime infer categories from
    /// the action name. When present, it is authoritative.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<Capability>,
    /// When an earlier identical result may be reused (see [`Reuse`]). State-changing
    /// actions are never reused, whatever this says.
    #[serde(default)]
    pub reuse: Reuse,
    /// Every call asks the person, whatever they allowed before: no "for this session" or
    /// "always" answer covers it. For actions nobody should run unattended (using the
    /// person's computer).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ask_each_time: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub person_only: bool,
    /// What one "for this session" or "always" answer covers, when it is not the action name:
    /// the calls of a group with the same values of some input fields (every Computer action in
    /// one app). See [`GrantKey`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant: Option<GrantKey>,
    /// This mutation is constrained to the selected project or contained runtime, so an
    /// automation may run it with nobody there to ask. False is the safe default. A chat in
    /// [`ExecutionMode::Auto`] does not need it: the person is there.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub auto_eligible: bool,
    /// Offered and runnable in [`ExecutionMode::Plan`] although it is `Mutate`. For an action
    /// that changes no project state, only the authority of the next turn, so that a person in
    /// Plan can still leave it. It must also be `person_only`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stays_in_plan: bool,
    /// The action's own text tells the model to close the turn with one short line ("then say
    /// in one line who is in it"). A turn that called it is not asked to continue when it ends
    /// on a one-line reply.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ends_with_short_reply: bool,
}

/// Splits approval answers by input: a grant covers the calls of `group` whose `fields` have
/// the same values. The approval class is `group:value` (`computer:whatsapp`), with several
/// fields joined by `@` (`process.start:npm run dev@web`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantKey {
    pub group: String,
    pub fields: Vec<String>,
    /// Values that differ only in case are the same (app names).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fold_case: bool,
}

impl GrantKey {
    /// The approval class of a call, or `None` when every field is missing or empty (the call
    /// then gets the default class). One empty field among others stays an empty value
    /// (`process.start:npm run dev@`), so a grant never widens to calls that left it out.
    pub fn class(&self, input: &serde_json::Value) -> Option<String> {
        let values: Vec<String> = self
            .fields
            .iter()
            .map(|f| {
                let v = match &input[f.as_str()] {
                    serde_json::Value::String(s) => {
                        s.split_whitespace().collect::<Vec<_>>().join(" ")
                    }
                    serde_json::Value::Null => String::new(),
                    other => other.to_string(),
                };
                if self.fold_case {
                    v.to_lowercase()
                } else {
                    v
                }
            })
            .collect();
        if values.iter().all(String::is_empty) {
            return None;
        }
        Some(format!("{}:{}", self.group, values.join("@")))
    }
}

impl ActionDescriptor {
    pub fn new(name: impl Into<String>, summary: impl Into<String>, risk: RiskTier) -> Self {
        ActionDescriptor {
            name: name.into(),
            summary: summary.into(),
            risk,
            // Always include `properties` (even empty): some tool-calling runtimes reject
            // an object schema without it.
            input_schema: serde_json::json!({ "type": "object", "properties": {} }),
            output_schema: serde_json::json!({ "type": "object" }),
            capabilities: Vec::new(),
            reuse: Reuse::Auto,
            ask_each_time: false,
            person_only: false,
            grant: None,
            auto_eligible: false,
            stays_in_plan: false,
            ends_with_short_reply: false,
        }
    }

    /// See [`ActionDescriptor::ends_with_short_reply`].
    pub fn ends_with_short_reply(mut self) -> Self {
        self.ends_with_short_reply = true;
        self
    }

    /// See [`ActionDescriptor::stays_in_plan`]. Implies [`person_only`](Self::person_only).
    pub fn stays_in_plan(mut self) -> Self {
        self = self.person_only();
        self.stays_in_plan = true;
        self
    }

    pub fn person_only(mut self) -> Self {
        self.person_only = true;
        self.ask_each_time = true;
        self
    }

    /// One answer covers the calls of `group` with the same values of `fields`
    /// ([`GrantKey`]).
    pub fn grant_per(mut self, group: &str, fields: &[&str]) -> Self {
        self.grant = Some(GrantKey {
            group: group.into(),
            fields: fields.iter().map(|f| f.to_string()).collect(),
            fold_case: false,
        });
        self
    }

    /// Like [`Self::grant_per`], with values that differ only in case taken as the same.
    pub fn grant_per_any_case(mut self, group: &str, fields: &[&str]) -> Self {
        self = self.grant_per(group, fields);
        if let Some(g) = &mut self.grant {
            g.fold_case = true;
        }
        self
    }

    /// Ask the person at every call; no earlier answer covers it ([`Self::ask_each_time`]).
    pub fn ask_each_time(mut self) -> Self {
        self.ask_each_time = true;
        self
    }

    /// Allow this project-bound mutation to run without a question in Auto mode.
    pub fn auto_eligible(mut self) -> Self {
        self.auto_eligible = true;
        self
    }

    /// The output depends only on the input: reuse it across turns ([`Reuse::Stable`]).
    pub fn reusable(mut self) -> Self {
        self.reuse = Reuse::Stable;
        self
    }

    /// Every call observes something new: never reuse it ([`Reuse::Volatile`]).
    pub fn volatile(mut self) -> Self {
        self.reuse = Reuse::Volatile;
        self
    }

    pub fn with_input_schema(mut self, schema: serde_json::Value) -> Self {
        self.input_schema = schema;
        self
    }

    pub fn with_output_schema(mut self, schema: serde_json::Value) -> Self {
        self.output_schema = schema;
        self
    }

    /// Declare the capability categories this action serves. Authoritative when set.
    pub fn with_capabilities(mut self, caps: impl IntoIterator<Item = Capability>) -> Self {
        self.capabilities = caps.into_iter().collect();
        self
    }

    /// The adapter portion of the action name (`fs` in `fs.read`).
    pub fn adapter_name(&self) -> &str {
        self.name.split('.').next().unwrap_or(&self.name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_grant_key_splits_answers_by_input_and_never_widens() {
        let d = ActionDescriptor::new("process.start", "start", RiskTier::Mutate)
            .grant_per("process.start", &["command", "folder"]);
        let g = d.grant.unwrap();
        assert_eq!(
            g.class(&json!({ "command": "npm  run dev", "folder": "web" }))
                .as_deref(),
            Some("process.start:npm run dev@web")
        );
        // One field left out stays empty: the grant covers only calls that leave it out too.
        assert_eq!(
            g.class(&json!({ "command": "npm run dev" })).as_deref(),
            Some("process.start:npm run dev@")
        );
        assert_eq!(g.class(&json!({})), None);
        let any = ActionDescriptor::new("app.use", "use", RiskTier::Mutate)
            .grant_per_any_case("app", &["app"])
            .grant
            .unwrap();
        assert_eq!(
            any.class(&json!({ "app": "WhatsApp" })).as_deref(),
            Some("app:whatsapp")
        );
    }

    #[test]
    fn auto_eligibility_is_explicit_and_old_descriptors_stay_manual() {
        let plain = ActionDescriptor::new("files.write", "write", RiskTier::Mutate);
        assert!(!plain.auto_eligible);
        assert!(plain.clone().auto_eligible().auto_eligible);
        let mut value = serde_json::to_value(plain).unwrap();
        value.as_object_mut().unwrap().remove("auto_eligible");
        let restored: ActionDescriptor = serde_json::from_value(value).unwrap();
        assert!(!restored.auto_eligible);
    }
}
