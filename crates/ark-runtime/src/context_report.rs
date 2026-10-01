//! What the model actually receives, broken down by source. The harness fills a
//! [`ContextReport`] each time it assembles a request, so a front-end can show where the
//! tokens go without guessing. Numbers are the same estimates the budget manager uses
//! (`ark_model::estimate_tokens`); the provider's measured prompt size is reported
//! separately on each `ModelCall` event.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ContextReport {
    /// The step within the turn this request belongs to (0 for a preview).
    pub step: usize,
    /// What the request is for: `reason` (a step), `plan`, `critic` or `synthesize` (the
    /// forced final answer). Empty in reports from before this field existed.
    #[serde(default)]
    pub role: String,
    /// The profile's persona and method block.
    pub instructions: u32,
    /// The runtime footer: project label, artifact list, approval note.
    pub project: u32,
    /// The one-line capability index (progressive disclosure keeps this small).
    pub capability_index: u32,
    /// The structured working state digest.
    pub working_state: u32,
    /// Bounded carry-over from earlier turns of the session.
    pub conversation: u32,
    /// What the user is looking at (focus and nearby evidence).
    pub workspace: u32,
    /// The user's message for this turn.
    pub task: u32,
    /// This turn's distilled tool results (and any steering) still inside the window.
    pub tool_rounds: u32,
    /// The model's own earlier output in the window: its tool calls and narration.
    #[serde(default)]
    pub model_output: u32,
    /// Evidence and knowledge the agent looked up this turn, still inside the window.
    #[serde(default)]
    pub retrieved: u32,
    /// Rounds dropped from the window on this request (their evidence stays retrievable).
    pub compressed_rounds: usize,
    /// Tool schemas sent alongside the prompt (meta-tools plus loaded actions). Reported
    /// separately because the budget manager does not count them.
    pub tool_schemas: u32,
    /// The part of `tool_schemas` that is loaded actions (the rest is the runtime's tools).
    #[serde(default)]
    pub action_schemas: u32,
    /// How many action schemas were loaded.
    pub loaded_actions: usize,
    /// Estimated prompt size the budget manager assembled (system plus messages).
    pub total: u32,
    /// The configured budget.
    pub budget: u32,
}

impl ContextReport {
    /// The labelled rows, in display order. Zero rows are kept so the shape is stable.
    pub fn rows(&self) -> Vec<(&'static str, u32)> {
        vec![
            ("Instructions", self.instructions),
            ("Project", self.project),
            ("Capability index", self.capability_index),
            ("Working state", self.working_state),
            ("Conversation", self.conversation),
            ("Workspace", self.workspace),
            ("Task", self.task),
            ("Tool results", self.tool_rounds),
            ("Model output", self.model_output),
            ("Retrieved context", self.retrieved),
        ]
    }
}
