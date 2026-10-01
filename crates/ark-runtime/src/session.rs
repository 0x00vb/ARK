//! Conversation sessions. A session is one continuing conversation inside a project: a
//! sequence of turns a human can leave and resume.
//!
//! The rule this module exists to enforce: **the transcript is not the context.** Every turn
//! is stored in full (so a front-end can show history and a human can audit it), but the model
//! only ever receives a bounded carry-over built by [`Session::conversation_digest`]:
//!
//! - the last few exchanges, each clipped;
//! - one-line gists of older turns, capped in number;
//! - nothing else. Durable knowledge lives in the project's working state and evidence
//!   cards, which the harness already feeds the model in bounded form.
//!
//! So a session with 500 turns sends about the same number of conversation tokens as one with
//! 5. [`Session::compact`] tightens the carry-over and the working state on demand, using the
//! same deterministic rules; it never asks a model to summarize the transcript.

use ark_core::{ProjectId, SessionId};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::engine::Engine;
use crate::error::AgentError;
use crate::harness::HarnessOutcome;
use crate::metrics::Metrics;

/// How much of the conversation rides along with each new turn.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CarryPolicy {
    /// Exchanges carried near-verbatim (clipped).
    pub recent_turns: usize,
    /// Characters kept from each carried user message.
    pub input_chars: usize,
    /// Characters kept from each carried answer.
    pub answer_chars: usize,
    /// One-line gists of older turns kept (newest win).
    pub max_gists: usize,
    /// Characters per gist.
    pub gist_chars: usize,
}

impl Default for CarryPolicy {
    fn default() -> Self {
        CarryPolicy {
            recent_turns: 3,
            input_chars: 300,
            answer_chars: 500,
            max_gists: 8,
            gist_chars: 160,
        }
    }
}

/// Cumulative accounting for a session.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SessionUsage {
    pub turns: u32,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    /// The part of `prompt_tokens` served from a provider's prompt cache.
    #[serde(default)]
    pub cached_tokens: u64,
    #[serde(default)]
    pub cache_write_tokens: u64,
    pub model_calls: u32,
    pub tool_calls: u32,
    pub duplicate_tool_calls: u32,
    pub peak_prompt_tokens: u32,
    /// False once any call's usage was estimated rather than measured.
    pub measured: bool,
    pub elapsed_ms: u64,
}

impl SessionUsage {
    fn add(&mut self, m: &Metrics) {
        if self.turns == 0 {
            self.measured = true;
        }
        self.turns += 1;
        self.prompt_tokens += m.prompt_tokens;
        self.completion_tokens += m.completion_tokens;
        self.cached_tokens += m.cached_tokens;
        self.cache_write_tokens += m.cache_write_tokens;
        self.model_calls += m.total_model_calls();
        self.tool_calls += m.tool_calls;
        self.duplicate_tool_calls += m.duplicate_tool_calls;
        self.peak_prompt_tokens = self.peak_prompt_tokens.max(m.peak_prompt_tokens);
        self.measured &= m.tokens_measured || m.total_model_calls() == 0;
        self.elapsed_ms += m.elapsed_ms;
    }
    pub fn total_tokens(&self) -> u64 {
        self.prompt_tokens + self.completion_tokens
    }
}

/// The persisted header of a session.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionRecord {
    pub id: SessionId,
    pub project: ProjectId,
    /// The first message, clipped: a human handle for the session list.
    pub title: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    pub turn_count: u32,
    /// Gists of turns that left the verbatim window, oldest first.
    #[serde(default)]
    pub gists: Vec<String>,
    /// Turns folded out of the gist list too (their conclusions live in the working state).
    #[serde(default)]
    pub dropped_turns: u32,
    #[serde(default)]
    pub usage: SessionUsage,
    #[serde(default)]
    pub compactions: u32,
    #[serde(default)]
    pub policy: CarryPolicy,
}

/// One stored exchange. The full text is kept for the human; the model sees a clip.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Turn {
    pub index: u32,
    pub input: String,
    pub answer: String,
    #[serde(default)]
    pub cited: Vec<String>,
    #[serde(default)]
    pub cancelled: bool,
    /// Messages the user sent while this turn ran that reached the model.
    #[serde(default)]
    pub steered: Vec<String>,
    #[serde(default)]
    pub metrics: Metrics,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    /// The turn did not finish: it failed, or the process stopped while it ran. `answer` is
    /// then empty and `error` says why.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub incomplete: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The process running the turn, while it runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
}

impl Turn {
    fn placeholder(index: u32, input: &str) -> Turn {
        Turn {
            index,
            input: input.to_string(),
            answer: String::new(),
            cited: Vec::new(),
            cancelled: false,
            steered: Vec::new(),
            metrics: Metrics::default(),
            at: OffsetDateTime::now_utc(),
            incomplete: true,
            error: None,
            pid: Some(std::process::id()),
        }
    }
}

/// Why a turn that never finished is marked so.
pub const INTERRUPTED: &str = "interrupted: the app stopped before this turn finished";

/// What a compaction changed, in estimated tokens.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CompactReport {
    pub conversation_before: u32,
    pub conversation_after: u32,
    pub state_before: u32,
    pub state_after: u32,
    /// Turns moved from the verbatim window into gists.
    pub turns_folded: usize,
    /// Gists merged away (their turns stay in the transcript and working state).
    pub gists_dropped: usize,
    /// Working-state items removed (duplicates, closed hypotheses, stale plans).
    pub state_items_removed: usize,
}

/// A live handle on one conversation session.
pub struct Session {
    record: SessionRecord,
    /// The last `policy.recent_turns` turns (the verbatim window), cached.
    recent: Vec<Turn>,
}

impl Session {
    /// Start a new, empty session in `project`.
    pub fn start(engine: &Engine, project: &ProjectId) -> Result<Session, AgentError> {
        Self::start_with(engine, project, CarryPolicy::default())
    }

    pub fn start_with(
        engine: &Engine,
        project: &ProjectId,
        policy: CarryPolicy,
    ) -> Result<Session, AgentError> {
        let now = OffsetDateTime::now_utc();
        let record = SessionRecord {
            id: SessionId::new(),
            project: project.clone(),
            title: String::new(),
            created_at: now,
            updated_at: now,
            turn_count: 0,
            gists: Vec::new(),
            dropped_turns: 0,
            usage: SessionUsage::default(),
            compactions: 0,
            policy,
        };
        let s = Session {
            record,
            recent: Vec::new(),
        };
        s.save(engine)?;
        Ok(s)
    }

    /// Reopen a session by id or unique id prefix.
    pub fn open(engine: &Engine, id: &str) -> Result<Option<Session>, AgentError> {
        let Some(v) = engine.store.resolve_session(id)? else {
            return Ok(None);
        };
        Self::load(engine, serde_json::from_value(v)?).map(Some)
    }

    /// The most recently active session in `project`, if any.
    pub fn latest(engine: &Engine, project: &ProjectId) -> Result<Option<Session>, AgentError> {
        match engine
            .store
            .list_sessions(Some(project))?
            .into_iter()
            .next()
        {
            None => Ok(None),
            Some(v) => Self::load(engine, serde_json::from_value(v)?).map(Some),
        }
    }

    /// Session headers, most recent first.
    pub fn list(
        engine: &Engine,
        project: Option<&ProjectId>,
    ) -> Result<Vec<SessionRecord>, AgentError> {
        engine
            .store
            .list_sessions(project)?
            .into_iter()
            .map(|v| serde_json::from_value(v).map_err(AgentError::from))
            .collect()
    }

    fn load(engine: &Engine, record: SessionRecord) -> Result<Session, AgentError> {
        // Turns already folded into gists are excluded from the window; a turn at or past
        // `turn_count` was begun and never recorded.
        let start = Self::window_start(&record);
        let count = record.turn_count;
        let read = Self::read_turns(engine, &record.id, Some(record.policy.recent_turns + 1))?;
        let begun = read.iter().find(|t| t.index >= count).cloned();
        let recent = read
            .into_iter()
            .filter(|t| t.index as usize >= start && t.index < count)
            .collect();
        let mut session = Session { record, recent };
        // A turn begun by a process that is gone was interrupted: keep it, marked, so the
        // person sees what they asked and the next turn's carry-over says it did not finish.
        if let Some(mut t) = begun {
            if !t.pid.is_some_and(crate::process::alive) {
                t.index = count;
                t.incomplete = true;
                t.error = Some(INTERRUPTED.to_string());
                t.pid = None;
                session.push_turn(engine, t, &Metrics::default())?;
            }
        }
        Ok(session)
    }

    /// Index of the first turn still in the verbatim window: every folded turn became a gist,
    /// and every dropped gist was counted.
    fn window_start(record: &SessionRecord) -> usize {
        (record.gists.len() + record.dropped_turns as usize).min(record.turn_count as usize)
    }

    /// Stored turns; one that cannot be read (a damaged record) is shown as such instead of
    /// making the whole session unreadable.
    fn read_turns(
        engine: &Engine,
        id: &SessionId,
        last: Option<usize>,
    ) -> Result<Vec<Turn>, AgentError> {
        Ok(engine
            .store
            .list_session_turns(id.as_str(), last)?
            .into_iter()
            .map(readable_turn)
            .collect())
    }

    fn save(&self, engine: &Engine) -> Result<(), AgentError> {
        engine.store.put_session(
            self.record.id.as_str(),
            &self.record.project,
            &serde_json::to_value(&self.record)?,
        )?;
        Ok(())
    }

    pub fn id(&self) -> &SessionId {
        &self.record.id
    }
    pub fn project(&self) -> &ProjectId {
        &self.record.project
    }
    pub fn record(&self) -> &SessionRecord {
        &self.record
    }
    pub fn usage(&self) -> &SessionUsage {
        &self.record.usage
    }
    pub fn turn_count(&self) -> u32 {
        self.record.turn_count
    }

    /// Give the session a human title (shown in session lists). Blank titles are ignored.
    pub fn set_title(&mut self, engine: &Engine, title: &str) -> Result<(), AgentError> {
        let t = clip(title, 80);
        if t.is_empty() {
            return Ok(());
        }
        self.record.title = t;
        self.save(engine)
    }

    /// Rename a stored session by id or prefix. Returns false when none matches. A live
    /// [`Session`] handle for the same id keeps its old title until reopened; rename that one
    /// with [`set_title`](Session::set_title).
    pub fn rename(engine: &Engine, id: &str, title: &str) -> Result<bool, AgentError> {
        match Self::open(engine, id)? {
            None => Ok(false),
            Some(mut s) => s.set_title(engine, title).map(|_| true),
        }
    }

    /// One page of the stored transcript, oldest first: up to `limit` turns before turn
    /// index `before` (the newest turns when `before` is `None`). For display only.
    pub fn transcript_page(
        &self,
        engine: &Engine,
        before: Option<u32>,
        limit: usize,
    ) -> Result<Vec<Turn>, AgentError> {
        let before = Some(
            before
                .unwrap_or(self.record.turn_count)
                .min(self.record.turn_count),
        );
        Ok(engine
            .store
            .list_session_turns_before(self.record.id.as_str(), before, limit)?
            .into_iter()
            .map(readable_turn)
            .collect())
    }

    /// The full stored transcript, for display. Never sent to a model. A turn still running
    /// is not part of it.
    pub fn transcript(
        &self,
        engine: &Engine,
        last: Option<usize>,
    ) -> Result<Vec<Turn>, AgentError> {
        let count = self.record.turn_count;
        Ok(
            Self::read_turns(engine, &self.record.id, last.map(|n| n + 1))?
                .into_iter()
                .filter(|t| t.index < count)
                .rev()
                .take(last.unwrap_or(usize::MAX))
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect(),
        )
    }

    /// The bounded carry-over for the next turn, or `None` for a fresh session.
    pub fn conversation_digest(&self) -> Option<String> {
        use std::fmt::Write as _;
        let p = &self.record.policy;
        if self.recent.is_empty() && self.record.gists.is_empty() {
            return None;
        }
        let mut s = String::new();
        if !self.record.gists.is_empty() || self.record.dropped_turns > 0 {
            let _ = writeln!(s, "Earlier turns (oldest first):");
            if self.record.dropped_turns > 0 {
                let _ = writeln!(
                    s,
                    "  (+{} earlier turns; their conclusions are in the working state)",
                    self.record.dropped_turns
                );
            }
            for g in &self.record.gists {
                let _ = writeln!(s, "  - {g}");
            }
        }
        if !self.recent.is_empty() {
            let _ = writeln!(s, "Most recent exchanges:");
            for t in &self.recent {
                let _ = writeln!(s, "  [user] {}", clip(&t.user_text(), p.input_chars));
                let reply = if t.cancelled {
                    "(cancelled before answering)".to_string()
                } else if t.incomplete {
                    format!(
                        "(did not finish: {})",
                        t.error.as_deref().unwrap_or("unknown reason")
                    )
                } else {
                    clip(&t.answer, p.answer_chars)
                };
                let _ = writeln!(s, "  [you] {reply}");
            }
        }
        Some(s.trim_end().to_string())
    }

    /// Record that a turn started, before it runs. If the process stops before
    /// [`record_turn`](Session::record_turn) or [`fail_turn`](Session::fail_turn), the next
    /// open marks the turn interrupted instead of losing it.
    pub fn begin_turn(&mut self, engine: &Engine, input: &str) -> Result<(), AgentError> {
        let t = Turn::placeholder(self.record.turn_count, input);
        engine.store.put_session_turn(
            self.record.id.as_str(),
            t.index,
            &serde_json::to_value(&t)?,
        )?;
        Ok(())
    }

    /// Store a finished turn and slide the carry-over window.
    pub fn record_turn(
        &mut self,
        engine: &Engine,
        input: &str,
        outcome: &HarnessOutcome,
    ) -> Result<(), AgentError> {
        let turn = Turn {
            index: self.record.turn_count,
            input: input.to_string(),
            answer: outcome.answer.clone(),
            cited: outcome
                .cited
                .iter()
                .map(|c| c.short().to_string())
                .collect(),
            cancelled: outcome.cancelled,
            steered: outcome.steered.clone(),
            metrics: outcome.metrics.clone(),
            at: OffsetDateTime::now_utc(),
            incomplete: false,
            error: None,
            pid: None,
        };
        self.push_turn(engine, turn, &outcome.metrics)
    }

    /// Store a turn that ended in an error, marked incomplete with the error.
    pub fn fail_turn(
        &mut self,
        engine: &Engine,
        input: &str,
        error: &str,
    ) -> Result<(), AgentError> {
        let mut t = Turn::placeholder(self.record.turn_count, input);
        t.error = Some(error.to_string());
        t.pid = None;
        self.push_turn(engine, t, &Metrics::default())
    }

    fn push_turn(
        &mut self,
        engine: &Engine,
        turn: Turn,
        metrics: &Metrics,
    ) -> Result<(), AgentError> {
        engine.store.put_session_turn(
            self.record.id.as_str(),
            turn.index,
            &serde_json::to_value(&turn)?,
        )?;
        if self.record.title.is_empty() {
            self.record.title = clip(&turn.input, 60);
        }
        self.record.turn_count += 1;
        self.record.usage.add(metrics);
        self.record.updated_at = OffsetDateTime::now_utc();
        self.recent.push(turn);
        let keep = self.record.policy.recent_turns;
        let max_gists = self.record.policy.max_gists;
        self.slide(keep, max_gists);
        self.save(engine)
    }

    /// Fold turns beyond `keep` into gists, and gists beyond `max_gists` into the dropped count.
    /// Returns (turns folded, gists dropped).
    fn slide(&mut self, keep: usize, max_gists: usize) -> (usize, usize) {
        let mut folded = 0;
        while self.recent.len() > keep {
            let t = self.recent.remove(0);
            self.record
                .gists
                .push(gist(&t, self.record.policy.gist_chars));
            folded += 1;
        }
        let mut dropped = 0;
        while self.record.gists.len() > max_gists {
            self.record.gists.remove(0);
            self.record.dropped_turns += 1;
            dropped += 1;
        }
        (folded, dropped)
    }

    /// Tighten the context on demand: keep only the latest exchange verbatim, halve the gist
    /// list, and tidy the working state. Deterministic; no model call. Nothing is deleted from
    /// the transcript or the evidence store.
    pub fn compact(&mut self, engine: &Engine) -> Result<CompactReport, AgentError> {
        use ark_model::estimate_tokens;
        let mut report = CompactReport {
            conversation_before: self
                .conversation_digest()
                .map(|d| estimate_tokens(&d))
                .unwrap_or(0),
            ..Default::default()
        };
        let (folded, dropped) = self.slide(1, (self.record.policy.max_gists / 2).max(1));
        report.turns_folded = folded;
        report.gists_dropped = dropped;
        report.conversation_after = self
            .conversation_digest()
            .map(|d| estimate_tokens(&d))
            .unwrap_or(0);

        let mut state = engine
            .store
            .get_research_state(&self.record.project)?
            .unwrap_or_default();
        report.state_before = estimate_tokens(&state.digest(crate::harness::STATE_DIGEST_ITEMS));
        report.state_items_removed = state.compact(6);
        report.state_after = estimate_tokens(&state.digest(crate::harness::STATE_DIGEST_ITEMS));
        engine
            .store
            .put_research_state(&self.record.project, &state)?;

        self.record.compactions += 1;
        self.record.updated_at = OffsetDateTime::now_utc();
        self.save(engine)?;
        Ok(report)
    }
}

impl Turn {
    /// What the user said in this turn: the message, plus anything sent while it ran.
    pub fn user_text(&self) -> String {
        if self.steered.is_empty() {
            self.input.clone()
        } else {
            format!("{} (then: {})", self.input, self.steered.join("; "))
        }
    }
}

/// A one-line summary of a turn: the question and the answer's first sentence.
/// A stored turn, or a marked stand-in when the record cannot be read.
fn readable_turn(v: serde_json::Value) -> Turn {
    let index = v["index"].as_u64().unwrap_or(0) as u32;
    let input = v["input"].as_str().unwrap_or_default().to_string();
    serde_json::from_value(v).unwrap_or_else(|e| {
        let mut t = Turn::placeholder(index, &input);
        t.pid = None;
        t.error = Some(format!("this turn's record could not be read ({e})"));
        t
    })
}

fn gist(t: &Turn, max: usize) -> String {
    let answer = if t.cancelled {
        "(cancelled)".to_string()
    } else if t.incomplete {
        "(did not finish)".to_string()
    } else {
        first_sentence(&t.answer)
    };
    let cites = if t.cited.is_empty() {
        String::new()
    } else {
        format!(
            " [inv:{}]",
            t.cited
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join(",")
        )
    };
    clip(
        &format!("{} → {}{}", flat(&t.user_text()), answer, cites),
        max,
    )
}

fn first_sentence(s: &str) -> String {
    let f = flat(s);
    match f.find(". ") {
        Some(i) => f[..=i].to_string(),
        None => f,
    }
}

fn flat(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn clip(s: &str, max: usize) -> String {
    let f = flat(s);
    if f.chars().count() <= max {
        f
    } else {
        format!(
            "{}…",
            f.chars().take(max.saturating_sub(1)).collect::<String>()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_core::ResearchState;
    use ark_tools::Registry;

    fn outcome(answer: &str) -> HarnessOutcome {
        HarnessOutcome {
            answer: answer.into(),
            cited: vec![],
            steps: 1,
            trivial: true,
            metrics: Metrics::start(),
            state: ResearchState::default(),
            cancelled: false,
            steered: vec![],
        }
    }

    #[test]
    fn digest_is_bounded_no_matter_how_long_the_session() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::open_with(dir.path(), Registry::new()).unwrap();
        let p = engine.create_project("p", "").unwrap();
        let mut s = Session::start(&engine, &p.id).unwrap();
        let mut sizes = Vec::new();
        for i in 0..300 {
            let q = format!("question {i}: {}", "please look at this again ".repeat(20));
            let a = format!("Answer {i}. {}", "Here is a long explanation. ".repeat(40));
            s.record_turn(&engine, &q, &outcome(&a)).unwrap();
            sizes.push(s.conversation_digest().unwrap().len());
        }
        let at_20 = sizes[19];
        let at_300 = sizes[299];
        assert!(
            at_300 <= at_20 + 64,
            "carry-over plateaus: turn 20 = {at_20} chars, turn 300 = {at_300} chars"
        );
        assert_eq!(
            s.transcript(&engine, None).unwrap().len(),
            300,
            "the full transcript is still stored"
        );
        assert!(s
            .conversation_digest()
            .unwrap()
            .contains("+289 earlier turns"));
    }

    #[test]
    fn resume_restores_the_window_without_replaying() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::open_with(dir.path(), Registry::new()).unwrap();
        let p = engine.create_project("p", "").unwrap();
        let mut s = Session::start(&engine, &p.id).unwrap();
        for i in 0..6 {
            s.record_turn(&engine, &format!("q{i}"), &outcome(&format!("a{i}.")))
                .unwrap();
        }
        let before = s.conversation_digest();
        let id = s.id().short().to_string();
        drop(s);
        let resumed = Session::open(&engine, &id).unwrap().unwrap();
        assert_eq!(
            resumed.conversation_digest(),
            before,
            "same carry-over after resume"
        );
        assert_eq!(
            Session::latest(&engine, &p.id).unwrap().unwrap().id(),
            resumed.id()
        );
        assert_eq!(resumed.turn_count(), 6);
    }

    #[test]
    fn rename_and_page_and_steered_input() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::open_with(dir.path(), Registry::new()).unwrap();
        let p = engine.create_project("p", "").unwrap();
        let mut s = Session::start(&engine, &p.id).unwrap();
        for i in 0..5 {
            let mut o = outcome(&format!("a{i}."));
            if i == 4 {
                o.steered = vec!["also check the door".into()];
            }
            s.record_turn(&engine, &format!("q{i}"), &o).unwrap();
        }
        assert!(s
            .conversation_digest()
            .unwrap()
            .contains("q4 (then: also check the door)"));
        let page = s.transcript_page(&engine, Some(4), 2).unwrap();
        assert_eq!(page.iter().map(|t| t.index).collect::<Vec<_>>(), vec![2, 3]);
        let newest = s.transcript_page(&engine, None, 2).unwrap();
        assert_eq!(
            newest.iter().map(|t| t.index).collect::<Vec<_>>(),
            vec![3, 4]
        );
        assert!(Session::rename(&engine, s.id().short(), "The door").unwrap());
        assert_eq!(
            Session::open(&engine, s.id().as_str())
                .unwrap()
                .unwrap()
                .record()
                .title,
            "The door"
        );
        assert!(!Session::rename(&engine, "nope", "x").unwrap());
    }

    #[test]
    fn compact_shrinks_carry_over_and_state() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::open_with(dir.path(), Registry::new()).unwrap();
        let p = engine.create_project("p", "").unwrap();
        let mut st = ResearchState::new("g");
        for _ in 0..3 {
            st.next_actions.push("read chapter 4".into());
        }
        engine.store.put_research_state(&p.id, &st).unwrap();
        let mut s = Session::start(&engine, &p.id).unwrap();
        for i in 0..12 {
            s.record_turn(
                &engine,
                &format!("question number {i}"),
                &outcome(&"an answer sentence. ".repeat(20)),
            )
            .unwrap();
        }
        let r = s.compact(&engine).unwrap();
        assert!(r.conversation_after < r.conversation_before, "{r:?}");
        assert_eq!(r.turns_folded, 2);
        assert_eq!(r.state_items_removed, 2);
        assert!(s.conversation_digest().unwrap().matches("[user]").count() == 1);
        // Resumed after compaction, the window stays compacted.
        let again = Session::open(&engine, s.id().as_str()).unwrap().unwrap();
        assert_eq!(again.conversation_digest(), s.conversation_digest());
    }
}
