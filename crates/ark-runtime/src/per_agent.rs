//! Tools each agent gets its own instance of: a web search on the agent's own model, a browser
//! context of its own. The conversation and every orchestrated agent ask the same
//! [`PerAgent`] sources for theirs when a turn or task starts, so a new capability of this
//! kind is one implementation, not wiring in each front-end and in the orchestrator.

use std::sync::Arc;

use ark_model::Provider;
use ark_tools::Adapter;

use crate::acquire::AcquisitionProvider;
use crate::distill::Distiller;

/// Who a turn's tools act for.
pub struct Owner<'a> {
    /// Stable for the conversation (`session:<id>`) or the agent (`<project>/<agent>`), so an
    /// agent finds its own state (its browser pages) again in its next task.
    pub name: &'a str,
    /// The model it reasons with.
    pub model: &'a Arc<dyn Provider>,
}

/// A source of tools made for one owner.
pub trait PerAgent: Send + Sync {
    /// The tools as a list shows them, for no owner in particular.
    fn listing(&self) -> Arc<dyn Adapter>;
    /// The tools of one owner.
    fn for_owner(&self, owner: &Owner) -> Arc<dyn Adapter>;
    /// Writes the evidence cards of their results.
    fn distiller(&self) -> Option<Distiller> {
        None
    }
    /// What one owner can ask to turn on mid-turn, when the source is off until the person
    /// allows it (the Computer, ADR 0045). Its tools are the owner's own, so the source is made
    /// per owner. Default: none.
    fn acquisition(&self, _owner: &Owner) -> Option<Arc<dyn AcquisitionProvider>> {
        None
    }
}
