//! A narrower authority than the person's grants for one run (ADR 0062). A [`CallGuard`] given
//! to a [`crate::Harness`] sees every call the model makes, meta tools included, before
//! anything else: before Plan mode, reuse and the approval gate. With a guard, standing grants,
//! `allow_mutate` and Auto mode do not apply to changes: each one the guard allows either asks
//! the person, every time, or runs unattended when the guard says so and records it first.

use ark_core::RiskTier;

/// What a guard says about a call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Not within the run's authority: the model is told why and nothing runs.
    Refuse(String),
    /// Runs: a read, or a change the authority lets run unattended.
    Allow,
    /// A change within the authority that the person approves, each time.
    Ask,
}

pub trait CallGuard: Send + Sync {
    /// `risk` is `None` for the runtime's own meta tools.
    fn check(&self, action: &str, input: &serde_json::Value, risk: Option<RiskTier>) -> Verdict;
    /// A change is about to run (approved, or allowed unattended): count it against its limits
    /// first, durably. An error refuses it.
    fn commit(&self, action: &str, input: &serde_json::Value) -> Result<(), String>;
    /// A call ended. `uncertain`: it may or may not have had its effect (an unconfirmed send).
    fn ended(&self, action: &str, risk: RiskTier, ok: bool, uncertain: bool);
}
