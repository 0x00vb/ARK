//! Context budget manager. Re-sending one growing `messages` vec on every completion makes
//! context unbounded and cost quadratic. Instead the transcript is a list of **rounds** (a round
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

/// Once over budget, old rounds are dropped until the messages fit in this share of the space
/// left. Dropping a chunk at once, instead of one round per step, keeps the start of the
/// message list unchanged for several steps, so a provider's prefix cache keeps serving it.
const REFILL: f32 = 0.7;

/// The budgeted working transcript.
pub struct WorkingSet {
    rounds: Vec<Vec<Message>>,
    /// Max prompt tokens (system + assembled messages) to target.
    budget: u32,
    /// How many oldest rounds were dropped on the last assemble. Only ever grows.
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

    /// Append a round (kept together forever, or dropped together). A round with a picture
    /// takes the pictures out of the rounds before it: only the newest picture of a window
    /// is worth its tokens, and the text of the older results stays.
    pub fn push_round(&mut self, round: Vec<Message>) {
        if round.is_empty() {
            return;
        }
        if round.iter().any(|m| !m.pictures.is_empty()) {
            for m in self.rounds.iter_mut().flatten() {
                m.pictures.clear();
            }
        }
        self.rounds.push(round);
    }

    /// Tokens of the model's own messages (tool calls, narration) in the rounds the last
    /// assemble kept.
    pub fn kept_model_output(&self) -> u32 {
        self.rounds
            .iter()
            .skip(self.last_compressed)
            .flatten()
            .filter(|m| m.role == ark_model::Role::Assistant)
            .map(Self::message_tokens)
            .sum()
    }

    /// A rough token cost for one message: its text plus a small structural overhead
    /// per tool call (name + serialized arguments).
    pub fn message_tokens(m: &Message) -> u32 {
        let mut t =
            estimate_tokens(&m.content) + 4 + m.pictures.len() as u32 * ark_model::PICTURE_TOKENS;
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
        self.assemble_with(system, 0)
    }

    /// Like [`assemble`](WorkingSet::assemble), keeping `reserve` tokens free for messages the
    /// caller appends after the rounds (the working state).
    pub fn assemble_with(&mut self, system: &str, reserve: u32) -> (Vec<Message>, u32) {
        let sys_tokens = estimate_tokens(system);
        let avail = self.budget.saturating_sub(sys_tokens + reserve);
        let costs: Vec<u32> = self.rounds.iter().map(|r| Self::round_tokens(r)).collect();
        let mut used: u32 = costs.iter().skip(self.last_compressed).sum();
        // Always keep at least the most recent round, even if it alone is over budget: the
        // model must see the latest tool results to proceed.
        let last = self.rounds.len().saturating_sub(1);
        if used > avail && self.last_compressed < last {
            let target = (avail as f32 * REFILL) as u32;
            while self.last_compressed < last && used > target {
                used -= costs[self.last_compressed];
                self.last_compressed += 1;
            }
        }
        let dropped = self.last_compressed;

        let mut out = Vec::new();
        if dropped > 0 {
            out.push(Message::user(format!(
                "[{dropped} earlier step(s) compressed — their conclusions are in the research state; \
                 retrieve any specific detail with get_evidence or search_evidence.]"
            )));
        }
        for round in self.rounds.iter().skip(dropped) {
            out.extend(round.iter().cloned());
        }
        let total = sys_tokens + used + reserve + if dropped > 0 { 40 } else { 0 };
        (out, total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_newest_picture_stays_and_it_counts() {
        let pic = || ark_model::Picture {
            media_type: "image/png".into(),
            bytes: std::sync::Arc::new(vec![1, 2, 3]),
        };
        let call = ark_model::ToolCall {
            id: "1".into(),
            name: "app.see".into(),
            arguments: serde_json::json!({}),
        };
        let mut ws = WorkingSet::new(100_000);
        ws.push_round(vec![
            Message::tool_result(&call, "first").with_pictures(vec![pic()])
        ]);
        let (_, one) = ws.assemble("");
        assert!(one >= ark_model::PICTURE_TOKENS);
        ws.push_round(vec![
            Message::tool_result(&call, "second").with_pictures(vec![pic()])
        ]);
        let (msgs, two) = ws.assemble("");
        assert!(msgs[0].pictures.is_empty(), "the older picture is gone");
        assert_eq!(msgs[0].content, "first", "its text stays");
        assert_eq!(msgs[1].pictures.len(), 1);
        assert!(two < 2 * ark_model::PICTURE_TOKENS);
    }
    use ark_model::{Message, ToolCall};

    fn round(text: &str) -> Vec<Message> {
        let call = ToolCall {
            id: "c1".into(),
            name: "files.read".into(),
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
    fn drops_in_chunks_so_the_prefix_stays_put() {
        let mut ws = WorkingSet::new(2000);
        let mut drops = Vec::new();
        for i in 0..30 {
            ws.push_round(round(&"word ".repeat(60 + i % 3)));
            let (msgs, total) = ws.assemble("sys");
            assert!(total <= 2000 + 400, "stays near the budget: {total}");
            drops.push(ws.last_compressed);
            assert!(msgs.last().unwrap().content.contains("word"));
        }
        // The dropped count only grows, and it grows in steps of several rounds: most
        // assemblies keep the same start as the one before.
        assert!(drops.windows(2).all(|w| w[1] >= w[0]));
        let changes = drops.windows(2).filter(|w| w[1] != w[0]).count();
        assert!(changes > 0 && changes <= 10, "dropped in chunks: {drops:?}");
    }

    #[test]
    fn reserve_leaves_room_for_the_tail() {
        let mut a = WorkingSet::new(1500);
        let mut b = WorkingSet::new(1500);
        for _ in 0..10 {
            a.push_round(round(&"x ".repeat(200)));
            b.push_round(round(&"x ".repeat(200)));
        }
        a.assemble_with("sys", 0);
        b.assemble_with("sys", 500);
        assert!(
            b.last_compressed > a.last_compressed,
            "a reserve drops more history"
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
