//! Actions: the typed operations adapters expose. Both the human and the agent invoke
//! tools only through these. An action *descriptor* is pure data (it lives in core); the
//! code that runs it lives in a tool adapter (`ark-tools`).

use crate::capability::Capability;
use serde::{Deserialize, Serialize};

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
    /// traffic, spend money. Always needs explicit human approval before it executes.
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
        }
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
