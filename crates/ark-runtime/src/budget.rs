//! Context budget manager. Re-sending one growing `messages` vec on every completion makes
//! context unbounded and cost quadratic. Instead the transcript is a pinned **head** (the task
//! frame: the goal, what the person said about it, what an earlier stopped turn did) and a list
//! of **rounds** (a round is an assistant tool-call turn plus its tool results, or a single
//! plain message). Every model call is assembled under a token budget: the head always, then
//! the newest rounds verbatim; the oldest rounds are dropped once they no longer fit.
//!
//! Dropping whole rounds (never half a round) keeps the tool-call protocol valid: a
//! `tool_result` never survives without the `tool_use` that spawned it. Dropped rounds leave a
//! **ledger** in their place (ADR 0050): one line per call they made, with its input and the
//! first line of its result, and the model's last note among them. The details stay in the
//! evidence store, retrievable with `get_evidence`/`search_evidence`. The head is never
//! dropped, so the task survives any number of steps, and a vendor session that rotates starts
//! from a request that still has it.

use ark_model::{estimate_tokens, Message, Role};

/// Once over budget, old rounds are dropped until the messages fit in this share of the space
/// left. Dropping a chunk at once, instead of one round per step, keeps the start of the
/// message list unchanged for several steps, so a provider's prefix cache keeps serving it.
const REFILL: f32 = 0.7;

/// Characters of a call's key input, of its result's first line, and of the model's last
/// note kept in the ledger.
const LEDGER_INPUT: usize = 60;
const LEDGER_RESULT: usize = 100;
const LEDGER_NOTE: usize = 600;

/// Inputs that name what a call worked on, in the order they are looked for.
const KEY_INPUTS: &[&str] = &[
    "path", "query", "command", "url", "name", "artifact", "id", "file",
];

/// The request budget when the model's window is unknown, and the smallest one ARK sizes.
pub const MIN_BUDGET: u32 = 6_000;
/// The largest request budget ARK sizes from a window.
pub const MAX_BUDGET: u32 = 48_000;

/// The request budget for a model with this input window (ADR 0050): a quarter of the
/// window, at least [`MIN_BUDGET`], at most [`MAX_BUDGET`], never over half the window.
/// Unknown windows keep [`MIN_BUDGET`].
pub fn budget_for(window: Option<u32>) -> u32 {
    match window {
        None => MIN_BUDGET,
        Some(w) => (w / 4).clamp(MIN_BUDGET, MAX_BUDGET).min(w / 2).max(1024),
    }
}

/// The budget `ARK_CTX_BUDGET` sets, if any.
pub fn budget_from_env() -> Option<u32> {
    std::env::var("ARK_CTX_BUDGET")
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

/// The budgeted working transcript.
pub struct WorkingSet {
    /// The task frame, sent first in every request and never dropped.
    head: Option<Message>,
    rounds: Vec<Vec<Message>>,
    /// Max prompt tokens (system + assembled messages) to target.
    budget: u32,
    /// How many oldest rounds were dropped on the last assemble. Only ever grows.
    pub last_compressed: usize,
    /// The ledger of the dropped rounds, rebuilt when more are dropped.
    ledger: Option<(usize, Message)>,
}

impl WorkingSet {
    pub fn new(budget: u32) -> Self {
        WorkingSet {
            head: None,
            rounds: Vec::new(),
            budget: budget.max(1024),
            last_compressed: 0,
            ledger: None,
        }
    }

    /// The task frame: sent first in every request, never dropped.
    pub fn pin(&mut self, head: Message) {
        self.head = Some(head);
    }

    /// Tokens of the pinned head.
    pub fn head_tokens(&self) -> u32 {
        self.head.as_ref().map(Self::message_tokens).unwrap_or(0)
    }

    /// Tokens of the ledger the last assemble sent (0 when nothing was dropped).
    pub fn ledger_tokens(&self) -> u32 {
        self.ledger
            .as_ref()
            .map(|(_, m)| Self::message_tokens(m))
            .unwrap_or(0)
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
            .filter(|m| m.role == Role::Assistant)
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

    /// Lines of ledger this budget affords.
    fn ledger_lines(&self) -> usize {
        (self.budget as usize / 400).clamp(10, 40)
    }

    /// What the ledger may cost, set aside once rounds start to be dropped.
    fn ledger_allowance(&self) -> u32 {
        self.ledger_lines() as u32 * 45 + estimate_tokens(&"x".repeat(LEDGER_NOTE)) + 60
    }

    /// Assemble the message list for one completion under the budget, given the
    /// system prompt that will accompany it. Returns the messages and the estimated
    /// total prompt tokens (system + messages).
    pub fn assemble(&mut self, system: &str) -> (Vec<Message>, u32) {
        self.assemble_with(system, 0)
    }

    /// Like [`assemble`](WorkingSet::assemble), keeping `reserve` tokens free for messages the
    /// caller appends after the rounds (the working state).
    ///
    /// The head is always sent, then the ledger of dropped rounds, then the kept rounds. When
    /// the system prompt and the head alone are over the budget, they are still sent with the
    /// newest round: the budget then bounds nothing else.
    pub fn assemble_with(&mut self, system: &str, reserve: u32) -> (Vec<Message>, u32) {
        let sys_tokens = estimate_tokens(system);
        let head_tokens = self.head_tokens();
        let avail = self
            .budget
            .saturating_sub(sys_tokens + reserve + head_tokens);
        let costs: Vec<u32> = self.rounds.iter().map(|r| Self::round_tokens(r)).collect();
        let mut used: u32 = costs.iter().skip(self.last_compressed).sum();
        // Always keep at least the most recent round, even if it alone is over budget: the
        // model must see the latest tool results to proceed.
        let last = self.rounds.len().saturating_sub(1);
        let ledger_room = if self.last_compressed > 0 {
            self.ledger_tokens()
        } else {
            0
        };
        if used + ledger_room > avail && self.last_compressed < last {
            let target = (avail.saturating_sub(self.ledger_allowance()) as f32 * REFILL) as u32;
            while self.last_compressed < last && used > target {
                used -= costs[self.last_compressed];
                self.last_compressed += 1;
            }
        }
        let dropped = self.last_compressed;
        if dropped > 0 && self.ledger.as_ref().map(|(n, _)| *n) != Some(dropped) {
            let text = ledger(&self.rounds[..dropped], self.ledger_lines());
            self.ledger = Some((dropped, Message::user(text)));
        }

        let mut out = Vec::new();
        if let Some(h) = &self.head {
            out.push(h.clone());
        }
        let ledger_tokens = if dropped > 0 {
            let (_, m) = self.ledger.as_ref().expect("built above");
            out.push(m.clone());
            Self::message_tokens(m)
        } else {
            0
        };
        for round in self.rounds.iter().skip(dropped) {
            out.extend(round.iter().cloned());
        }
        let total = sys_tokens + head_tokens + ledger_tokens + used + reserve;
        (out, total)
    }
}

/// What the dropped `rounds` did, for the model: one line per call (its action, key input and
/// the first line of its result), the newest `max` kept in order, and the model's last note
/// among them. Deterministic: the same rounds give the same text, so the prefix cache keeps it.
fn ledger(rounds: &[Vec<Message>], max: usize) -> String {
    use std::collections::HashMap;
    use std::fmt::Write as _;
    let mut calls: HashMap<&str, (&str, String)> = HashMap::new();
    let mut lines: Vec<String> = Vec::new();
    let mut note: Option<&str> = None;
    for m in rounds.iter().flatten() {
        match m.role {
            Role::Assistant => {
                if !m.content.trim().is_empty() {
                    note = Some(m.content.trim());
                }
                for c in &m.tool_calls {
                    calls.insert(c.id.as_str(), (c.name.as_str(), key_input(&c.arguments)));
                }
            }
            Role::Tool => {
                let id = m.tool_call_id.as_deref().unwrap_or("");
                let (name, input) = calls
                    .get(id)
                    .map(|(n, i)| (n.to_string(), i.clone()))
                    .unwrap_or_else(|| (m.tool_name.clone().unwrap_or_default(), String::new()));
                let (inv, result) = card_line(&m.content);
                let mut line = format!("- {name}");
                if !input.is_empty() {
                    let _ = write!(line, " {input}");
                }
                if !result.is_empty() {
                    let _ = write!(line, " → {}", clip(&result, LEDGER_RESULT));
                }
                if let Some(inv) = inv {
                    let _ = write!(line, " [{inv}]");
                }
                lines.push(line);
            }
            _ => {}
        }
    }
    let mut s = String::from(
        "[Earlier steps of this task, no longer shown in full. What they did, in order \
         (get_evidence reads any result again):",
    );
    let older = lines.len().saturating_sub(max);
    if older > 0 {
        let _ = write!(s, "\n  (+{older} earlier calls)");
    }
    for l in &lines[older..] {
        let _ = write!(s, "\n{l}");
    }
    if lines.is_empty() {
        s.push_str("\n  (no calls)");
    }
    if let Some(n) = note {
        let _ = write!(
            s,
            "\nYour last note in those steps: \"{}\"",
            clip(n, LEDGER_NOTE)
        );
    }
    s.push(']');
    s
}

/// The input that names what a call worked on (`path=src/a.rs`), clipped.
pub(crate) fn key_input(args: &serde_json::Value) -> String {
    for k in KEY_INPUTS {
        if let Some(v) = args.get(*k).and_then(|v| v.as_str()) {
            if !v.trim().is_empty() {
                return format!("{k}={}", clip(v.trim(), LEDGER_INPUT));
            }
        }
    }
    String::new()
}

/// A result card's `inv:` id and the first line of its summary. A card reads
/// `[inv:abcd] action: summary...`; other results give their first line.
fn card_line(content: &str) -> (Option<String>, String) {
    let first = content.lines().next().unwrap_or("").trim();
    if let Some(rest) = first.strip_prefix('[') {
        if let Some((inv, after)) = rest.split_once("] ") {
            if inv.starts_with("inv:") {
                let summary = after.split_once(": ").map(|(_, s)| s).unwrap_or(after);
                return (Some(inv.to_string()), summary.trim().to_string());
            }
        }
    }
    (None, first.to_string())
}

fn clip(s: &str, max: usize) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() <= max {
        s
    } else {
        format!("{}…", s.chars().take(max).collect::<String>())
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
            msgs[0].content.starts_with("[Earlier steps of this task"),
            "the ledger stands in for the dropped rounds"
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

    fn read(id: &str, path: &str, result: &str) -> Vec<Message> {
        let call = ToolCall {
            id: id.into(),
            name: "files.read".into(),
            arguments: serde_json::json!({"path": path}),
        };
        let mut said = Message::assistant_tool_calls(vec![call.clone()]);
        said.content = format!("Reading {path} next.");
        vec![said, Message::tool_result(&call, result)]
    }

    #[test]
    fn the_head_is_never_dropped() {
        let mut ws = WorkingSet::new(2000);
        ws.pin(Message::user(
            "== TASK ==\nadd syntax highlighting to the editor",
        ));
        for i in 0..200 {
            ws.push_round(read(
                &format!("c{i}"),
                &format!("src/f{i}.js"),
                &format!(
                    "[inv:{i:04}] files.read: src/f{i}.js (lines 1-80 of 900)\n{}",
                    "code ".repeat(300)
                ),
            ));
            let (msgs, _) = ws.assemble("sys");
            assert!(msgs[0].content.contains("syntax highlighting"), "step {i}");
        }
        assert!(ws.last_compressed > 150);
    }

    #[test]
    fn the_ledger_says_what_dropped_rounds_did() {
        let mut ws = WorkingSet::new(2000);
        ws.pin(Message::user("the task"));
        ws.push_round(read(
            "a",
            "web/js/file.js",
            "[inv:aa11] files.read: web/js/file.js (lines 1-120 of 400)\nconst x = 1;",
        ));
        ws.push_round(read(
            "b",
            "web/app.css",
            "[inv:bb22] files.read: web/app.css (lines 1-80 of 2000)\n.wb-editor {}",
        ));
        for i in 0..6 {
            ws.push_round(round(&"filler ".repeat(600 + i)));
        }
        let (msgs, total) = ws.assemble("sys");
        let ledger = &msgs[1].content;
        assert!(
            ledger.contains(
                "- files.read path=web/js/file.js → web/js/file.js (lines 1-120 of 400) [inv:aa11]"
            ),
            "{ledger}"
        );
        assert!(ledger.contains("path=web/app.css"), "{ledger}");
        assert!(ledger.contains("Your last note in those steps"), "{ledger}");
        assert!(ws.ledger_tokens() > 0);
        assert!(total >= ws.head_tokens() + ws.ledger_tokens());
    }

    #[test]
    fn the_ledger_is_bounded_and_keeps_the_newest_calls() {
        let mut ws = WorkingSet::new(4000);
        for i in 0..500 {
            ws.push_round(read(
                &format!("c{i}"),
                &format!("f{i}"),
                &format!("[inv:{i:04}] files.read: {}", "long summary ".repeat(50)),
            ));
        }
        let (msgs, _) = ws.assemble("sys");
        let ledger = &msgs[0].content;
        assert_eq!(ledger.lines().filter(|l| l.starts_with("- ")).count(), 10);
        assert!(ledger.contains("earlier calls)"));
        let newest_dropped = ws.last_compressed - 1;
        assert!(
            ledger.contains(&format!("path=f{newest_dropped} ")),
            "{ledger}"
        );
        assert!(
            ws.ledger_tokens() < 1200,
            "bounded: {} tokens",
            ws.ledger_tokens()
        );
    }

    #[test]
    fn the_ledger_changes_only_when_more_rounds_drop() {
        let mut ws = WorkingSet::new(3000);
        ws.pin(Message::user("task"));
        let mut seen = Vec::new();
        for i in 0..40 {
            ws.push_round(round(&"word ".repeat(150 + i % 3)));
            let (msgs, _) = ws.assemble("sys");
            if ws.last_compressed > 0 {
                seen.push((ws.last_compressed, msgs[1].content.clone()));
            }
        }
        for w in seen.windows(2) {
            if w[0].0 == w[1].0 {
                assert_eq!(w[0].1, w[1].1, "same drops, same ledger");
            }
        }
    }

    #[test]
    fn an_oversized_head_is_still_sent_with_the_newest_round() {
        let mut ws = WorkingSet::new(1024);
        ws.pin(Message::user("goal ".repeat(3000)));
        ws.push_round(round("first"));
        ws.push_round(round("second"));
        let (msgs, total) = ws.assemble("sys");
        assert!(msgs[0].content.starts_with("goal"));
        assert!(msgs.last().unwrap().content.contains("second"));
        assert!(total > 1024);
    }

    #[test]
    fn the_budget_follows_the_window() {
        assert_eq!(
            budget_for(None),
            6_000,
            "unknown windows keep the old budget"
        );
        assert_eq!(budget_for(Some(200_000)), 48_000, "Claude");
        assert_eq!(budget_for(Some(272_000)), 48_000, "Codex");
        assert_eq!(
            budget_for(Some(64_000)),
            16_000,
            "a vendor session's rotation limit"
        );
        assert_eq!(budget_for(Some(128_000)), 32_000);
        assert_eq!(
            budget_for(Some(8_192)),
            4_096,
            "never over half a small window"
        );
        assert_eq!(budget_for(Some(32_768)), 8_192);
    }
}
