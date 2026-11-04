//! Agent introspection metrics. An investigation is a budget of tokens, model calls
//! and tool calls; you cannot improve what you do not measure. These are the numbers
//! we compare before/after the harness redesign, and that the CLI prints at the end
//! of `wb ask`.

use std::collections::BTreeMap;

use ark_model::Usage;
use serde::{Deserialize, Serialize};

use crate::router::Role;

/// The stable lowercase name of a role, as used in metrics keys and events.
pub fn role_name(r: Role) -> &'static str {
    match r {
        Role::Classify => "classify",
        Role::Plan => "plan",
        Role::Summarize => "summarize",
        Role::Select => "select",
        Role::Extract => "extract",
        Role::Reason => "reason",
        Role::Synthesize => "synthesize",
        Role::Critic => "critic",
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Metrics {
    /// Model calls, keyed by role.
    pub model_calls: BTreeMap<String, u32>,
    /// Model calls, keyed by the concrete model id (cost attribution).
    pub calls_by_model: BTreeMap<String, u32>,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    /// The part of `prompt_tokens` served from a provider's prompt cache.
    #[serde(default)]
    pub cached_tokens: u64,
    /// The part of `prompt_tokens` written to a provider's prompt cache.
    #[serde(default)]
    pub cache_write_tokens: u64,
    /// Whether every usage number so far was measured by the provider (vs estimated).
    pub tokens_measured: bool,
    /// Adapter tool calls actually executed.
    pub tool_calls: u32,
    /// Tool calls skipped because identical evidence already existed (dedup wins).
    pub duplicate_tool_calls: u32,
    /// Evidence cards created fresh this run.
    pub cards_created: u32,
    /// Prior evidence reused (retrieval / dedup hits) instead of regenerated.
    pub evidence_reused: u32,
    /// Meta-tool retrievals (get_evidence / search_evidence).
    pub retrievals: u32,
    /// Research-state edits the model made.
    pub state_updates: u32,
    pub steps: u32,
    /// The largest single-call prompt we sent, in tokens — the context-size headline.
    pub peak_prompt_tokens: u32,
    /// Model calls tried again after a temporary failure.
    #[serde(default)]
    pub provider_retries: u32,
    /// Wall-clock milliseconds.
    pub elapsed_ms: u64,
    #[serde(skip)]
    started: Option<std::time::Instant>,
}

impl Metrics {
    pub fn accumulate(&mut self, newer: &Metrics) {
        for (key, value) in &newer.model_calls {
            *self.model_calls.entry(key.clone()).or_default() += value;
        }
        for (key, value) in &newer.calls_by_model {
            *self.calls_by_model.entry(key.clone()).or_default() += value;
        }
        self.prompt_tokens += newer.prompt_tokens;
        self.completion_tokens += newer.completion_tokens;
        self.cached_tokens += newer.cached_tokens;
        self.cache_write_tokens += newer.cache_write_tokens;
        self.tokens_measured &= newer.tokens_measured;
        self.tool_calls += newer.tool_calls;
        self.duplicate_tool_calls += newer.duplicate_tool_calls;
        self.cards_created += newer.cards_created;
        self.evidence_reused += newer.evidence_reused;
        self.retrievals += newer.retrievals;
        self.state_updates += newer.state_updates;
        self.steps += newer.steps;
        self.peak_prompt_tokens = self.peak_prompt_tokens.max(newer.peak_prompt_tokens);
        self.provider_retries += newer.provider_retries;
        self.elapsed_ms += newer.elapsed_ms;
    }
    pub fn start() -> Self {
        Metrics {
            tokens_measured: true,
            started: Some(std::time::Instant::now()),
            ..Default::default()
        }
    }

    /// Record one model call: its role and the tokens it cost.
    pub fn record_call(&mut self, role: Role, model_id: &str, usage: Usage) {
        *self.model_calls.entry(role_name(role).into()).or_insert(0) += 1;
        *self.calls_by_model.entry(model_id.into()).or_insert(0) += 1;
        self.prompt_tokens += usage.prompt_tokens as u64;
        self.completion_tokens += usage.completion_tokens as u64;
        self.cached_tokens += usage.cached_tokens as u64;
        self.cache_write_tokens += usage.cache_write_tokens as u64;
        if !usage.measured {
            self.tokens_measured = false;
        }
        self.peak_prompt_tokens = self.peak_prompt_tokens.max(usage.prompt_tokens);
    }

    pub fn record_tool_call(&mut self) {
        self.tool_calls += 1;
    }
    pub fn record_duplicate(&mut self) {
        self.duplicate_tool_calls += 1;
        self.evidence_reused += 1;
    }
    pub fn record_card_created(&mut self) {
        self.cards_created += 1;
    }
    pub fn record_retrieval(&mut self) {
        self.retrievals += 1;
        self.evidence_reused += 1;
    }
    pub fn record_state_update(&mut self) {
        self.state_updates += 1;
    }
    pub fn record_retry(&mut self) {
        self.provider_retries += 1;
    }
    pub fn record_step(&mut self) {
        self.steps += 1;
    }

    pub fn total_tokens(&self) -> u64 {
        self.prompt_tokens + self.completion_tokens
    }
    pub fn total_model_calls(&self) -> u32 {
        self.model_calls.values().sum()
    }

    /// Retrieval hit rate: reused evidence over all evidence touched (reused + fresh).
    pub fn reuse_rate(&self) -> f32 {
        let touched = self.evidence_reused + self.cards_created;
        if touched == 0 {
            0.0
        } else {
            self.evidence_reused as f32 / touched as f32
        }
    }

    /// Freeze the wall clock. Call once at the end.
    pub fn finish(&mut self) {
        if let Some(t) = self.started.take() {
            self.elapsed_ms = t.elapsed().as_millis() as u64;
        }
    }

    /// Tokens per confirmed finding — the efficiency number that matters most.
    pub fn tokens_per_finding(&self, findings: usize) -> u64 {
        if findings == 0 {
            self.total_tokens()
        } else {
            self.total_tokens() / findings as u64
        }
    }

    /// A one-block human summary for the CLI.
    pub fn summary(&self, findings: usize) -> String {
        use std::fmt::Write as _;
        let mut s = String::new();
        let approx = if self.tokens_measured { "" } else { "~" };
        let _ = writeln!(
            s,
            "tokens: {approx}{} ({}prompt / {}completion), peak prompt {}",
            self.total_tokens(),
            self.prompt_tokens,
            self.completion_tokens,
            self.peak_prompt_tokens
        );
        let _ = writeln!(
            s,
            "model calls: {} ({}), tool calls: {} ({} duplicate avoided)",
            self.total_model_calls(),
            self.calls_by_model
                .iter()
                .map(|(m, n)| format!("{m}×{n}"))
                .collect::<Vec<_>>()
                .join(", "),
            self.tool_calls,
            self.duplicate_tool_calls
        );
        let _ = writeln!(
            s,
            "steps: {}, retrievals: {}, evidence reuse: {:.0}%, tokens/finding: {approx}{}",
            self.steps,
            self.retrievals,
            self.reuse_rate() * 100.0,
            self.tokens_per_finding(findings)
        );
        if self.elapsed_ms > 0 {
            let _ = writeln!(s, "elapsed: {:.1}s", self.elapsed_ms as f64 / 1000.0);
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accumulates_tokens_and_calls() {
        let mut m = Metrics::start();
        m.record_call(Role::Reason, "strong", Usage::measured(1000, 200));
        m.record_call(Role::Reason, "strong", Usage::measured(500, 100));
        m.record_call(Role::Plan, "fast", Usage::measured(300, 50));
        assert_eq!(m.total_tokens(), 2150);
        assert_eq!(m.total_model_calls(), 3);
        assert_eq!(m.model_calls["reason"], 2);
        assert_eq!(m.calls_by_model["strong"], 2);
        assert_eq!(m.peak_prompt_tokens, 1000);
        assert!(m.tokens_measured);
    }

    #[test]
    fn estimated_usage_marks_unmeasured() {
        let mut m = Metrics::start();
        m.record_call(
            Role::Reason,
            "x",
            Usage {
                prompt_tokens: 10,
                completion_tokens: 2,
                measured: false,
                cached_tokens: 0,
                cache_write_tokens: 0,
            },
        );
        assert!(!m.tokens_measured);
        assert!(m.summary(1).contains("~12"));
    }

    #[test]
    fn reuse_rate_and_dedup() {
        let mut m = Metrics::start();
        m.record_card_created();
        m.record_card_created();
        m.record_duplicate(); // one reuse
        assert!((m.reuse_rate() - 1.0 / 3.0).abs() < 1e-6);
        assert_eq!(m.duplicate_tool_calls, 1);
        assert_eq!(m.tokens_per_finding(0), m.total_tokens());
    }
}
