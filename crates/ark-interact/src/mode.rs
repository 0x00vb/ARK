//! The chat's own tool for its execution mode (ADR 0104). The person asks in words; the model
//! calls `mode.set`; the person approves; the mode applies when the turn has ended.

use std::sync::{Arc, Mutex};

use ark_sdk::prelude::ark_core::{ActionDescriptor, ExecutionMode, RiskTier};
use ark_sdk::prelude::ark_tools::{ActionOutput, Adapter, AdapterError, RunCtx};
use ark_sdk::prelude::TurnTools;
use async_trait::async_trait;
use serde_json::{json, Value};

pub(crate) const SET: &str = "mode.set";

/// The mode a call asked for, kept until the turn ends.
pub(crate) type Requested = Arc<Mutex<Option<ExecutionMode>>>;

pub(crate) fn turn_tools(current: ExecutionMode, requested: Requested) -> TurnTools {
    TurnTools {
        adapter: Arc::new(ModeTool { current, requested }),
        // Whole: the model must read that the change comes next message and tell the person.
        distiller: Some(Arc::new(|action: &str, out: &Value| {
            (action == SET).then(|| (out["result"].as_str().unwrap_or("").to_string(), Vec::new()))
        })),
    }
}

struct ModeTool {
    /// The mode this turn runs in. It does not change while the turn runs.
    current: ExecutionMode,
    requested: Requested,
}

fn name(mode: ExecutionMode) -> &'static str {
    match mode {
        ExecutionMode::Plan => "plan",
        ExecutionMode::Manual => "manual",
        ExecutionMode::Auto => "auto",
    }
}

#[async_trait]
impl Adapter for ModeTool {
    fn name(&self) -> &str {
        "mode"
    }

    fn actions(&self) -> Vec<ActionDescriptor> {
        let mut action = ActionDescriptor::new(
            SET,
            "Set this chat's mode for its next message: plan (no changes), manual (ask before changes) or auto. Only when the person asks.",
            RiskTier::Mutate,
        )
        .stays_in_plan()
        .ends_with_short_reply();
        action.input_schema = json!({
            "type": "object",
            "properties": { "mode": { "type": "string", "enum": ["plan", "manual", "auto"] } },
            "required": ["mode"]
        });
        vec![action]
    }

    async fn run(
        &self,
        _action: &str,
        input: Value,
        _ctx: &RunCtx<'_>,
    ) -> ark_sdk::prelude::ark_tools::Result<ActionOutput> {
        let mode: ExecutionMode =
            serde_json::from_value(input["mode"].clone()).map_err(|_| AdapterError::BadInput {
                action: SET.into(),
                detail: "mode is plan, manual or auto".into(),
            })?;
        let said = if mode == self.current {
            format!("This chat is already in {} mode.", name(mode))
        } else {
            *self.requested.lock().unwrap() = Some(mode);
            format!(
                "Approved. The chat switches to {} mode from the person's next message. This turn keeps {}. Tell the person.",
                name(mode),
                name(self.current)
            )
        };
        Ok(ActionOutput::json(json!({ "result": said })))
    }
}
