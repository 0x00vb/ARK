//! Context budget manager. The old loop kept a single `messages` vec that grew with
//! every turn and was re-sent in full on every completion — context grew unbounded
//! and cost grew quadratically. Here the transcript is a list of **rounds** (a round
//! is an assistant tool-call turn plus its tool results, or a single plain message),
//! and every model call is assembled under a token budget: the newest rounds are kept
//! verbatim, the oldest are dropped once they no longer fit.
//!
//! Dropping whole rounds (never half a round) keeps the tool-call protocol valid — a
//! `tool_result` never survives without the `tool_use` that spawned it. The details
//! in a dropped round are not lost: they live on as evidence cards and in the
//! research-state digest, retrievable on demand with `get_evidence`/`search_evidence`.
//! The result is an active context that stays roughly bounded no matter how many
//! actions the investigation takes.

use ark_model::{estimate_tokens, Message};

/// The budgeted working transcript.
pub struct WorkingSet {
    rounds: Vec<Vec<Message>>,
    /// Max prompt tokens (system + assembled messages) to target.
    budget: u32,
    /// How many oldest rounds were dropped on the last assemble.
    pub last_compressed: usize,
}

impl WorkingSet {
    pub fn new(budget: u32) -> Self {
        WorkingSet {
            rounds: Vec::new(),
            budget: budget.max(1024),
            last_compressed: 0,
        }
    }

    /// Append a round (kept together forever, or dropped together).
    pub fn push_round(&mut self, round: Vec<Message>) {
        if !round.is_empty() {
            self.rounds.push(round);
        }
    }

    pub fn round_count(&self) -> usize {
        self.rounds.len()
    }

    /// A rough token cost for one message: its text plus a small structural overhead
    /// per tool call (name + serialized arguments).
    fn message_tokens(m: &Message) -> u32 {
        let mut t = estimate_tokens(&m.content) + 4;
        for c in &m.tool_calls {
            t += estimate_tokens(&c.name) + estimate_tokens(&c.arguments.to_string()) + 8;
        }
        t
    }

    fn round_tokens(round: &[Message]) -> u32 {
        round.iter().map(Self::message_tokens).sum()
    }

    /// Assemble the message list for one completion under the budget, given the
    /// system prompt that will accompany it. Returns the messages and the estimated
    /// total prompt tokens (system + messages).
    pub fn assemble(&mut self, system: &str) -> (Vec<Message>, u32) {
        let sys_tokens = estimate_tokens(system);
        let avail = self.budget.saturating_sub(sys_tokens);

        // Walk newest→oldest, keeping rounds while they fit.
        let mut kept_rev: Vec<&Vec<Message>> = Vec::new();
        let mut used = 0u32;
        let mut dropped = 0usize;
        for (i, round) in self.rounds.iter().enumerate().rev() {
            let cost = Self::round_tokens(round);
            if used + cost <= avail || kept_rev.is_empty() {
                // Always keep at least the most recent round, even if it alone is
                // over budget — the model must see the latest tool results to proceed.
                used += cost;
                kept_rev.push(round);
            } else {
                dropped = i + 1;
                break;
            }
        }
        self.last_compressed = dropped;

        let mut out = Vec::new();
        if dropped > 0 {
            out.push(Message::user(format!(
                "[{dropped} earlier step(s) compressed — their conclusions are in the research state above; \
                 retrieve any specific detail with get_evidence or search_evidence.]"
            )));
        }
        for round in kept_rev.into_iter().rev() {
            out.extend(round.iter().cloned());
        }
        let total = sys_tokens + used + if dropped > 0 { 40 } else { 0 };
        (out, total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_model::{Message, ToolCall};

    fn round(text: &str) -> Vec<Message> {
        let call = ToolCall {
            id: "c1".into(),
            name: "r2.decompile".into(),
            arguments: serde_json::json!({"artifact":"x"}),
        };
        vec![
            Message::assistant_tool_calls(vec![call.clone()]),
            Message::tool_result(&call, text),
        ]
    }

    #[test]
    fn keeps_everything_under_budget() {
        let mut ws = WorkingSet::new(100_000);
        ws.push_round(round("small result a"));
        ws.push_round(round("small result b"));
        let (msgs, _t) = ws.assemble("system");
        assert_eq!(ws.last_compressed, 0);
        // 2 rounds × 2 messages, no compression note
        assert_eq!(msgs.len(), 4);
    }

    #[test]
    fn drops_oldest_rounds_over_budget_and_notes_it() {
        // tiny budget forces compression; big payloads per round
        let mut ws = WorkingSet::new(1024); // clamped minimum
        for i in 0..8 {
            ws.push_round(round(&"lorem ipsum ".repeat(60 + i)));
        }
        let (msgs, total) = ws.assemble(&"sys ".repeat(50));
        assert!(ws.last_compressed > 0, "older rounds dropped");
        assert!(
            msgs[0].content.contains("compressed"),
            "compression note prepended"
        );
        // The most recent round is always retained.
        assert!(msgs.last().unwrap().content.contains("lorem"));
        assert!(
            total <= 1024 + 2000,
            "assembled prompt stays near the budget"
        );
    }

    #[test]
    fn always_keeps_latest_round_even_if_oversized() {
        let mut ws = WorkingSet::new(1024);
        ws.push_round(round(&"x".repeat(20_000))); // one huge round
        let (msgs, _t) = ws.assemble("sys");
        assert!(
            msgs.iter().any(|m| m.content.len() > 10_000),
            "latest round kept despite size"
        );
    }
}
