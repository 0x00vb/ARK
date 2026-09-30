//! Dynamic model routing. Not every step of an investigation needs the strongest
//! model. Cheap/fast models are fine for classification, planning, summarizing and
//! extracting; the strong model earns its cost on reasoning over evidence, forming
//! vulnerability hypotheses, and synthesizing the final answer. Deterministic code
//! does whatever it can so no model call is spent on it at all (distillation, dedup,
//! citation checks live outside this module).
//!
//! Two tiers keep configuration simple; a single-model setup routes every role to
//! the one backend, so nothing here forces a second model on anyone.
//!
//! Env:
//! - `ARK_MODEL_FAST` / `ARK_PROVIDER_FAST` — the cheap tier (else falls back to the
//!   default provider from `ARK_PROVIDER`/`ARK_MODEL`).
//! - `ARK_MODEL_STRONG` / `ARK_PROVIDER_STRONG` — the strong tier (same fallback).

use std::sync::Arc;

use ark_model::{Provider, ProviderError};

/// What a given model call is for. The router maps each to a tier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// Sort a question into trivial vs investigation (usually deterministic; here for completeness).
    Classify,
    /// Draft a short investigation plan.
    Plan,
    /// Compress stale context into a running summary.
    Summarize,
    /// Pick which tool/action to use.
    Select,
    /// Pull structured identifiers out of text.
    Extract,
    /// Reason over gathered evidence and drive the tool loop.
    Reason,
    /// Produce the final, cited synthesis.
    Synthesize,
    /// Adversarially check a finished investigation.
    Critic,
}

impl Role {
    /// Whether this role wants the strong tier.
    pub fn wants_strong(&self) -> bool {
        matches!(self, Role::Reason | Role::Synthesize | Role::Critic)
    }
}

/// Routes roles to providers across two tiers.
pub struct ModelRouter {
    fast: Arc<dyn Provider>,
    strong: Arc<dyn Provider>,
}

impl ModelRouter {
    pub fn new(fast: Arc<dyn Provider>, strong: Arc<dyn Provider>) -> Self {
        ModelRouter { fast, strong }
    }

    /// A cheap clone that shares the same underlying providers (the `Arc`s are cloned, not the
    /// models). Lets a facade hand a fresh router to each `Harness` run.
    pub fn clone_view(&self) -> ModelRouter {
        ModelRouter { fast: self.fast.clone(), strong: self.strong.clone() }
    }

    /// Both tiers share one provider (a single-model setup).
    pub fn single(provider: Arc<dyn Provider>) -> Self {
        ModelRouter { fast: provider.clone(), strong: provider }
    }

    /// Build from the environment. Falls back to the default provider for whichever
    /// tier is not explicitly configured, so `ARK_PROVIDER=ollama` alone still works.
    pub fn from_env() -> Result<Self, ProviderError> {
        let default = ark_model::from_env_arc()?;
        let fast = match tier_from_env("FAST") {
            Some(r) => r?,
            None => default.clone(),
        };
        let strong = match tier_from_env("STRONG") {
            Some(r) => r?,
            None => default.clone(),
        };
        Ok(ModelRouter { fast, strong })
    }

    /// The provider to use for a role.
    pub fn provider(&self, role: Role) -> &Arc<dyn Provider> {
        if role.wants_strong() {
            &self.strong
        } else {
            &self.fast
        }
    }

    pub fn describe(&self) -> String {
        if Arc::ptr_eq(&self.fast, &self.strong) {
            format!("{} (single tier)", self.strong.describe())
        } else {
            format!("fast={} · strong={}", self.fast.describe(), self.strong.describe())
        }
    }

    /// The model id backing a role, for metrics.
    pub fn model_id(&self, role: Role) -> String {
        self.provider(role).model_id()
    }
}

/// Build one tier from `ARK_{PROVIDER,MODEL}_{suffix}` if either is set.
fn tier_from_env(suffix: &str) -> Option<Result<Arc<dyn Provider>, ProviderError>> {
    let provider = std::env::var(format!("ARK_PROVIDER_{suffix}")).ok();
    let model = std::env::var(format!("ARK_MODEL_{suffix}")).ok();
    match (provider, model) {
        (None, None) => None,
        (p, m) => {
            let which = p.unwrap_or_else(|| std::env::var("ARK_PROVIDER").unwrap_or_else(|_| "ollama".into()));
            Some(ark_model::build(&which, m))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_model::MockProvider;

    #[test]
    fn single_tier_routes_all_roles_to_one_provider() {
        let p: Arc<dyn Provider> = Arc::new(MockProvider::new("solo", vec![]));
        let r = ModelRouter::single(p);
        assert_eq!(r.model_id(Role::Classify), "solo");
        assert_eq!(r.model_id(Role::Synthesize), "solo");
        assert!(r.describe().contains("single tier"));
    }

    #[test]
    fn two_tiers_split_by_role() {
        let fast: Arc<dyn Provider> = Arc::new(MockProvider::new("fast-m", vec![]));
        let strong: Arc<dyn Provider> = Arc::new(MockProvider::new("strong-m", vec![]));
        let r = ModelRouter::new(fast, strong);
        assert_eq!(r.model_id(Role::Classify), "fast-m");
        assert_eq!(r.model_id(Role::Plan), "fast-m");
        assert_eq!(r.model_id(Role::Reason), "strong-m");
        assert_eq!(r.model_id(Role::Synthesize), "strong-m");
        assert!(Role::Critic.wants_strong());
        assert!(!Role::Extract.wants_strong());
    }
}
