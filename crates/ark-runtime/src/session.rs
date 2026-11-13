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
//!
//! Work a chat set up (a scheduled run) reports back with a [`Post`]: a message in the chat's
//! own stream, apart from its numbered turns, so posting never races a turn. The next turn
//! carries the posts that arrived since the last answer, at most [`POSTS_CARRIED`], clipped.

use ark_core::{ExecutionMode, ProjectId, SessionId};
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_scope: Option<ProjectId>,
    /// Its coordinator settles begun turns from a separate execution journal.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub execution_managed: bool,
    /// Made for work an agent handed to the background (`work.later`, ADR 0118): the person
    /// never opened it, so no list of the person's chats shows it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub background: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    pub turn_count: u32,
    /// The exact model spec explicitly selected for this chat. Older and default-routed chats
    /// have no lossless single-model selection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
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
    /// The last post ([`Post`]) a turn has carried; later ones are new to the model.
    #[serde(default)]
    pub posts_read: u64,
    /// Authority selected for the next turn in this chat.
    #[serde(default)]
    pub execution_mode: ExecutionMode,
    /// Changes whenever a client changes `execution_mode`.
    #[serde(default)]
    pub execution_mode_revision: u64,
}

impl SessionRecord {
    /// A chat nobody chose a mode for (revision 0) is Auto, whichever mode an older version
    /// stored for it. A person's choice has a revision above 0 and stays. Applied wherever a
    /// stored record becomes the live one, so the stored copy is settled the next time it is
    /// written.
    fn settled(mut self) -> Self {
        if self.execution_mode_revision == 0 {
            self.execution_mode = ExecutionMode::Auto;
        }
        self
    }

    pub fn empty(
        id: SessionId,
        project: ProjectId,
        policy: CarryPolicy,
        model: Option<String>,
    ) -> Self {
        let now = OffsetDateTime::now_utc();
        Self {
            id,
            project,
            title: String::new(),
            work_item: None,
            execution_scope: None,
            execution_managed: false,
            background: false,
            created_at: now,
            updated_at: now,
            turn_count: 0,
            model,
            gists: Vec::new(),
            dropped_turns: 0,
            usage: SessionUsage::default(),
            compactions: 0,
            policy,
            posts_read: 0,
            // Nobody has chosen: Auto, until a person asks for Plan or Manual in words.
            execution_mode: ExecutionMode::Auto,
            execution_mode_revision: 0,
        }
    }
}

pub const PENDING_RESULT: &str = "session_result_pending";

pub fn artifact_references(input: &str) -> Result<Vec<String>, AgentError> {
    let mut refs = Vec::new();
    for part in input.split("art:").skip(1) {
        let reference: String = part
            .chars()
            .take_while(char::is_ascii_hexdigit)
            .take(65)
            .collect();
        if !(8..=64).contains(&reference.len()) || refs.contains(&reference) {
            continue;
        }
        refs.push(reference);
        if refs.len() > 32 {
            return Err(AgentError::NotAllowed(
                "pass at most 32 files in one message".into(),
            ));
        }
    }
    Ok(refs)
}

/// Posts a turn carries at most; older unread ones are counted, not shown.
pub const POSTS_CARRIED: usize = 3;

/// How a post reads in its chat.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PostKind {
    /// A result of work the chat set up: a card with a title.
    #[default]
    Result,
    /// The agent's own next message: drawn as a turn of that agent, no card (ADR 0118).
    Message,
}

impl PostKind {
    pub fn is_result(&self) -> bool {
        *self == PostKind::Result
    }
}

/// A message posted into a chat by work the chat set up, not by the person in it: an agent's
/// scheduled run reporting its result.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Post {
    /// Who posts: the agent's name.
    pub from: String,
    /// What it is about, in a few words.
    pub title: String,
    pub text: String,
    /// False when the work failed; `text` says why.
    pub ok: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    /// The chat's last stored turn when it was posted: a transcript shows it after that turn.
    #[serde(default)]
    pub after: Option<u32>,
    /// The work it reports on (a task id), so a result is never posted twice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work: Option<String>,
    /// A result card or the agent's own message. A post stored before this field reads as a result.
    #[serde(default, skip_serializing_if = "PostKind::is_result")]
    pub kind: PostKind,
}

impl Post {
    pub fn new(
        from: impl Into<String>,
        title: impl Into<String>,
        text: impl Into<String>,
        ok: bool,
    ) -> Post {
        Post {
            from: from.into(),
            title: title.into(),
            text: text.into(),
            ok,
            at: OffsetDateTime::now_utc(),
            after: None,
            work: None,
            kind: PostKind::Result,
        }
    }

    /// Read as the agent's own next message, not a result card.
    pub fn message(mut self) -> Post {
        self.kind = PostKind::Message;
        self
    }

    /// The work it reports on: [`posted`] finds it by this.
    pub fn work(mut self, id: impl Into<String>) -> Post {
        self.work = Some(id.into());
        self
    }
}

fn posts_stream(session: &str) -> String {
    format!("session-posts/{session}")
}

/// Post into the chat `session`, open or not. Returns the post's sequence number.
pub fn post(engine: &Engine, session: &str, post: Post) -> Result<u64, AgentError> {
    post_once(engine, session, post).map(|(sequence, _)| sequence)
}

/// Retain one copy of a work result, including through concurrent publication and restart.
/// A later result for the same work has a separate identity when its content changes.
pub fn post_once(
    engine: &Engine,
    session: &str,
    mut post: Post,
) -> Result<(u64, bool), AgentError> {
    let header = engine.store.get_session(session)?.ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "the result's destination conversation is unavailable",
        )
    })?;
    let header: SessionRecord = serde_json::from_value(header)?;
    let key = format!(
        "post-{}",
        blake3::hash(&serde_json::to_vec(&(
            session, &post.work, &post.text, post.ok
        ))?)
        .to_hex()
    );
    engine
        .store
        .transact_records(header.project.as_str(), |tx| {
            let header: SessionRecord =
                serde_json::from_value(tx.get_session(session)?.ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "the result's destination conversation was deleted",
                    )
                })?)?;
            if post.work.is_some() {
                if let Some(receipt) = tx.get_record("session_post_receipt", &key)? {
                    let sequence = receipt["sequence"].as_u64().ok_or_else(|| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "the result publication receipt is invalid",
                        )
                    })?;
                    return Ok((sequence, false));
                }
            }
            if post.work.is_some() {
                let fields = [
                    ("work", serde_json::to_value(&post.work)?),
                    ("text", serde_json::to_value(&post.text)?),
                    ("ok", serde_json::to_value(post.ok)?),
                ];
                if let Some(sequence) = tx.event_with_fields(&posts_stream(session), &fields)? {
                    tx.insert_record(
                        "session_post_receipt",
                        &key,
                        &serde_json::json!({"sequence":sequence}),
                    )?;
                    return Ok((sequence, false));
                }
            }
            post.after = header.turn_count.checked_sub(1);
            let sequence =
                tx.append_event(&posts_stream(session), &serde_json::to_value(&post)?)?;
            if post.work.is_some() {
                tx.insert_record(
                    "session_post_receipt",
                    &key,
                    &serde_json::json!({"sequence":sequence}),
                )?;
            }
            Ok((sequence, true))
        })
}

/// Whether `session` has a post about the work `work` ([`Post::work`]).
pub fn posted(engine: &Engine, session: &str, work: &str) -> Result<bool, AgentError> {
    let mut after = 0;
    loop {
        let page = posts(engine, session, after, 500)?;
        if page.iter().any(|(_, p)| p.work.as_deref() == Some(work)) {
            return Ok(true);
        }
        match page.last() {
            Some((seq, _)) if page.len() == 500 => after = *seq,
            _ => return Ok(false),
        }
    }
}

/// Up to `limit` posts of `session` after sequence number `after`, oldest first.
pub fn posts(
    engine: &Engine,
    session: &str,
    after: u64,
    limit: usize,
) -> Result<Vec<(u64, Post)>, AgentError> {
    Ok(engine
        .store
        .list_events(&posts_stream(session), after, limit)?
        .into_iter()
        .filter_map(|(seq, v)| serde_json::from_value(v).ok().map(|p| (seq, p)))
        .collect())
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
    /// Images the person attached: project files, in the order shown.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<String>,
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
    /// When that process started, to tell it from a later one with the same pid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid_start: Option<u64>,
    /// What this turn works on when it continues an unfinished one (ADR 0050): the first
    /// request of the chain and what the person said about it since. `None`: its own input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<Goal>,
    /// It answered because it reached its step limit, not because the work was done
    /// ([`HarnessOutcome::ran_out`]): the next turn continues its goal.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ran_out: bool,
    /// Authority frozen when this turn began.
    #[serde(default)]
    pub execution_mode: ExecutionMode,
}

/// Follow-ups a goal keeps in full, newest kept, and the characters of each.
const FOLLOW_UPS: usize = 8;
const FOLLOW_UP_CHARS: usize = 500;

/// What a chain of turns works on (ADR 0050): the request that started it and every later
/// message about it, from follow-up turns and from steering, newest last. A turn that
/// continues an unfinished one takes its goal and adds its own message, so "continue" never
/// becomes the goal and no instruction is lost.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Goal {
    /// The person's first message of the chain, as the app passed it (it may be empty when
    /// only files were sent).
    pub request: String,
    /// Later messages about it, newest last; the newest [`FOLLOW_UPS`] are kept.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub follow_ups: Vec<String>,
    /// Older follow-ups no longer kept.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub earlier: u32,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

impl Goal {
    pub fn new(request: impl Into<String>) -> Goal {
        Goal {
            request: request.into(),
            follow_ups: Vec::new(),
            earlier: 0,
        }
    }

    /// Add a later message, keeping the newest [`FOLLOW_UPS`].
    pub fn then(&mut self, message: &str) {
        let m = message.trim();
        if m.is_empty() {
            return;
        }
        self.follow_ups.push(clip(m, FOLLOW_UP_CHARS));
        while self.follow_ups.len() > FOLLOW_UPS {
            self.follow_ups.remove(0);
            self.earlier += 1;
        }
    }

    /// For the model: the request (clipped to `request_chars`) and the follow-ups, the newest
    /// that fit (`request_chars / 500`, 2 to 8, each at most `request_chars / 8` characters),
    /// saying to follow the newest where they differ and to do what the newest asks if it asks
    /// for something else.
    pub fn render(&self, request_chars: usize) -> String {
        use std::fmt::Write as _;
        let mut s = format!(
            "The person asked for this (it is not finished yet):\n{}",
            clip_lines(&self.request, request_chars)
        );
        if !self.follow_ups.is_empty() || self.earlier > 0 {
            let shown = (request_chars / 500).clamp(2, FOLLOW_UPS);
            let each = (request_chars / 8).clamp(80, FOLLOW_UP_CHARS);
            let older = self.follow_ups.len().saturating_sub(shown) + self.earlier as usize;
            s.push_str("\nThen they said, newest last (follow the newest where they differ):");
            if older > 0 {
                let _ = write!(s, "\n  (+{older} earlier messages)");
            }
            let from = self.follow_ups.len().saturating_sub(shown);
            for f in &self.follow_ups[from..] {
                let _ = write!(s, "\n  - {}", clip(f, each));
            }
            s.push_str(
                "\nIf their newest message asks for something else, do that instead; the task above stays unfinished.",
            );
        }
        s
    }

    /// The request and the follow-ups as one text, for choosing rules, skills and tools.
    pub fn text(&self) -> String {
        let mut s = self.request.clone();
        for f in &self.follow_ups {
            s.push('\n');
            s.push_str(f);
        }
        s
    }
}

impl Turn {
    /// What this turn worked on, with what the person said while it ran.
    pub fn effective_goal(&self) -> Goal {
        let mut g = self.goal.clone().unwrap_or_else(|| Goal::new(&self.input));
        for m in &self.steered {
            g.then(m);
        }
        g
    }

    /// Whether this turn did not finish (it failed, the process stopped, or the person
    /// stopped it): the next turn continues it.
    pub fn unfinished(&self) -> bool {
        self.incomplete || self.cancelled || self.ran_out
    }

    fn placeholder(index: u32, input: &str) -> Turn {
        Turn {
            index,
            input: input.to_string(),
            answer: String::new(),
            cited: Vec::new(),
            cancelled: false,
            steered: Vec::new(),
            images: Vec::new(),
            metrics: Metrics::default(),
            at: OffsetDateTime::now_utc(),
            incomplete: true,
            error: None,
            pid: Some(std::process::id()),
            pid_start: crate::process::own_start(),
            goal: None,
            ran_out: false,
            execution_mode: ExecutionMode::Manual,
        }
    }
}

/// Why a turn that never finished is marked so.
pub const INTERRUPTED: &str = "interrupted: the app stopped before this turn finished";

/// Unfinished turns in a row a continuation looks back over for their calls (ADR 0050). The
/// goal itself goes back through the whole chain: each turn stores it.
const UNFINISHED_TURNS: usize = 20;
/// Characters of the model's last note and of the reason a turn stopped, carried.
const NOTE_CARRIED: usize = 600;
const ERROR_CARRIED: usize = 300;

/// The unfinished turns at the end of a conversation and the calls they made
/// ([`Session::unfinished`]).
#[derive(Clone, Debug, Default)]
pub struct Unfinished {
    pub turns: Vec<u32>,
    /// Their work names ([`Session::work_of`]), as their calls and the records tools made
    /// for them carry them.
    pub works: Vec<String>,
    /// Oldest first.
    pub calls: Vec<ark_core::Invocation>,
    /// One line per call, as [`Unfinished::calls`]: its action, key input and the first line
    /// of its result card.
    pub lines: Vec<String>,
    /// The goal of the last unfinished turn, with what the person said while it ran.
    pub goal: Goal,
    /// Why the last turn stopped (its error), if it said.
    pub error: Option<String>,
    /// Whether the person stopped it.
    pub stopped: bool,
}

impl Unfinished {
    /// What the turns did, for the model: why the last one stopped, one line per call (the
    /// newest that fit `budget`: `budget / 600`, 5 to 20), what the tools keep from them
    /// (`held`, [`Engine::continued`]), the model's last note, and what to do with them.
    pub fn carry(&self, held: &[String], budget: u32) -> String {
        use ark_core::InvocationStatus as S;
        use std::fmt::Write as _;
        let max = (budget as usize / 600).clamp(5, 20);
        let mut s = String::from(
            "Your last turn stopped before it finished. The calls it made are below: do not make \
             a change marked done again; continue from where it stopped, unless the person's \
             newest message asks for something else.",
        );
        if self.stopped {
            s.push_str("\nThe person stopped it.");
        } else if let Some(e) = &self.error {
            let _ = write!(s, "\nIt stopped because: {}", clip(e, ERROR_CARRIED));
        }
        for h in held.iter().take(max) {
            let _ = write!(s, "\n  - {}", clip(h, 240));
        }
        let ran: Vec<(&ark_core::Invocation, &String)> = self
            .calls
            .iter()
            .zip(self.lines.iter())
            .filter(|(c, _)| c.status != S::PendingApproval)
            .collect();
        if ran.is_empty() {
            if held.is_empty() {
                s.push_str("\n  (it made no calls)");
            }
        } else {
            let older = ran.len().saturating_sub(max);
            if older > 0 {
                let _ = write!(s, "\n  (+{older} earlier calls)");
            }
            for (c, line) in &ran[older..] {
                let state = match c.status {
                    S::Ok => "done",
                    S::Failed if c.error.as_deref() == Some(crate::engine::INTERRUPTED_CALL) => {
                        if c.risk == ark_core::RiskTier::Mutate {
                            "cut short (it may or may not have happened)"
                        } else {
                            "cut short"
                        }
                    }
                    S::Failed => "failed",
                    _ => "cut short",
                };
                let _ = write!(s, "\n  - {state}: {line}");
            }
        }
        if let Some(n) = self
            .calls
            .iter()
            .rev()
            .find_map(|c| c.narration.as_deref().filter(|n| !n.trim().is_empty()))
        {
            let _ = write!(
                s,
                "\nYour last note before it stopped: \"{}\"",
                clip(n, NOTE_CARRIED)
            );
        }
        s
    }

    /// Changes that were running when the process stopped: whether they happened is unknown.
    pub fn cut_short(&self) -> Vec<ark_core::Invocation> {
        self.calls
            .iter()
            .filter(|c| {
                // Running: the person's stop dropped it under way (the process lives on).
                c.risk == ark_core::RiskTier::Mutate
                    && (c.status == ark_core::InvocationStatus::Running
                        || c.error.as_deref() == Some(crate::engine::INTERRUPTED_CALL))
            })
            .cloned()
            .collect()
    }
}

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
#[derive(Clone)]
pub struct Session {
    record: SessionRecord,
    /// The last `policy.recent_turns` turns (the verbatim window), cached.
    recent: Vec<Turn>,
    /// Posts after `record.posts_read`, as of the last [`Session::read_posts`].
    unread: Vec<(u64, Post)>,
    /// The goal of the turn under way, stored with it however it ends.
    goal: Option<Goal>,
    /// What the person said while the turn under way ran ([`Session::steered_now`]), kept
    /// with it however it ends.
    steered: Vec<String>,
    /// The images of the turn under way, stored with it however it ends.
    images: Vec<String>,
    /// Authority copied into the begun turn. A later mode change cannot alter it.
    turn_execution_mode: Option<ExecutionMode>,
}

impl Session {
    /// Start a new, empty session in `project`.
    pub fn start(engine: &Engine, project: &ProjectId) -> Result<Session, AgentError> {
        Self::start_with(engine, project, CarryPolicy::default())
    }

    /// Start a new, empty session with an identity chosen by a trusted caller.
    pub fn start_named(
        engine: &Engine,
        id: SessionId,
        project: &ProjectId,
    ) -> Result<Session, AgentError> {
        Self::start_named_with_model(engine, id, project, CarryPolicy::default(), None)
    }

    pub fn start_named_model(
        engine: &Engine,
        id: SessionId,
        project: &ProjectId,
        model: String,
    ) -> Result<Session, AgentError> {
        Self::start_named_with_model(engine, id, project, CarryPolicy::default(), Some(model))
    }

    pub fn start_with(
        engine: &Engine,
        project: &ProjectId,
        policy: CarryPolicy,
    ) -> Result<Session, AgentError> {
        Self::start_named_with_model(engine, SessionId::new(), project, policy, None)
    }

    fn start_named_with_model(
        engine: &Engine,
        id: SessionId,
        project: &ProjectId,
        policy: CarryPolicy,
        model: Option<String>,
    ) -> Result<Session, AgentError> {
        let record = SessionRecord::empty(id, project.clone(), policy, model);
        let s = Session {
            record,
            recent: Vec::new(),
            unread: Vec::new(),
            goal: None,
            steered: Vec::new(),
            images: Vec::new(),
            turn_execution_mode: None,
        };
        engine.store.create_session(
            s.record.id.as_str(),
            &s.record.project,
            &serde_json::to_value(&s.record)?,
        )?;
        Ok(s)
    }

    /// Reopen a session by id or unique id prefix.
    pub fn open(engine: &Engine, id: &str) -> Result<Option<Session>, AgentError> {
        let Some(v) = engine.store.resolve_session(id)? else {
            return Ok(None);
        };
        Self::load(engine, serde_json::from_value(v)?, true).map(Some)
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
            Some(v) => Self::load(engine, serde_json::from_value(v)?, true).map(Some),
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

    /// An admitted execution repairs its own result journal before advancing an unfinished turn.
    pub fn open_for_execution(engine: &Engine, id: &str) -> Result<Option<Session>, AgentError> {
        let Some(value) = engine.store.resolve_session(id)? else {
            return Ok(None);
        };
        let record: SessionRecord = serde_json::from_value(value)?;
        if record.work_item.is_none() {
            return Err(AgentError::NotAllowed(
                "execution recovery requires a work conversation".into(),
            ));
        }
        Self::load(engine, record, false).map(Some)
    }

    fn load(
        engine: &Engine,
        record: SessionRecord,
        close_interrupted: bool,
    ) -> Result<Session, AgentError> {
        let record = record.settled();
        // Turns already folded into gists are excluded from the window; a turn at or past
        // `turn_count` was begun and never recorded.
        let close_interrupted = close_interrupted && !record.execution_managed;
        let start = Self::window_start(&record);
        let count = record.turn_count;
        let read = Self::read_turns(engine, &record.id, Some(record.policy.recent_turns + 1))?;
        let pending = if record.work_item.is_some() {
            engine
                .store
                .transact_records::<_, AgentError>(record.project.as_str(), |tx| {
                    Ok(tx
                        .get_record(PENDING_RESULT, &format!("{}/{}", record.id, count))?
                        .map(serde_json::from_value::<Turn>)
                        .transpose()?)
                })?
        } else {
            None
        };
        let begun = pending.or_else(|| read.iter().find(|t| t.index >= count).cloned());
        let recent = read
            .into_iter()
            .filter(|t| t.index as usize >= start && t.index < count)
            .collect();
        let mut session = Session {
            record,
            recent,
            unread: Vec::new(),
            goal: None,
            steered: Vec::new(),
            images: Vec::new(),
            turn_execution_mode: None,
        };
        if let Some(mut t) = begun {
            session.turn_execution_mode = Some(t.execution_mode);
            match t.pid {
                // Stored in full (finished, or failed with its error), and the process stopped
                // before the header counted it: count it as it is.
                None => {
                    // The posts it carried are read, as when it was counted on time.
                    session.read_posts(engine)?;
                    let metrics = t.metrics.clone();
                    session.push_turn(engine, t, &metrics)?;
                }
                // A turn begun by a process that is gone was interrupted: keep it, marked, so
                // the person sees what they asked and the next turn continues it.
                Some(pid)
                    if close_interrupted && !crate::process::writer_alive(pid, t.pid_start) =>
                {
                    t.index = count;
                    t.incomplete = true;
                    t.error = Some(INTERRUPTED.to_string());
                    t.pid = None;
                    t.pid_start = None;
                    session.push_turn(engine, t, &Metrics::default())?;
                }
                Some(_) => {}
            }
        }
        // After marking an interrupted turn: it never carried them.
        session.read_posts(engine)?;
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

    pub fn prepare_artifacts(&self, engine: &Engine, input: &str) -> Result<(), AgentError> {
        let scope = self.execution_project(engine)?;
        if scope == self.record.project {
            return Ok(());
        }
        for reference in artifact_references(input)? {
            if engine.store.resolve_artifact(&scope, &reference)?.is_some() {
                continue;
            }
            let artifact = engine
                .store
                .resolve_artifact(&self.record.project, &reference)?
                .ok_or_else(|| AgentError::ArtifactNotFound(reference.clone()))?;
            engine.store.put_artifact(&scope, &artifact)?;
        }
        Ok(())
    }

    pub fn refresh_context(&mut self, engine: &Engine) -> Result<(), AgentError> {
        let current: SessionRecord = serde_json::from_value(
            engine
                .store
                .get_session(self.id().as_str())?
                .ok_or_else(|| AgentError::NotAllowed("the conversation was deleted".into()))?,
        )?;
        if current.project != self.record.project
            || self
                .record
                .execution_scope
                .as_ref()
                .is_some_and(|scope| current.execution_scope.as_ref() != Some(scope))
            || self
                .record
                .work_item
                .as_ref()
                .is_some_and(|work| current.work_item.as_ref() != Some(work))
        {
            return Err(AgentError::NotAllowed(
                "the conversation's work context changed".into(),
            ));
        }
        self.record.execution_scope = current.execution_scope;
        self.record.work_item = current.work_item;
        self.execution_project(engine)?;
        Ok(())
    }

    pub fn execution_project(&self, engine: &Engine) -> Result<ProjectId, AgentError> {
        let Some(scope) = &self.record.execution_scope else {
            return Ok(self.record.project.clone());
        };
        engine
            .store
            .transact_records::<_, AgentError>(self.record.project.as_str(), |tx| {
                tx.validate_execution_project(scope.as_str())?;
                let binding = tx
                    .get_record("session_execution_scope", scope.as_str())?
                    .ok_or_else(|| {
                        AgentError::NotAllowed(
                            "the conversation's work context is unavailable".into(),
                        )
                    })?;
                if binding["session"] != self.id().as_str()
                    || binding["work"].as_str() != self.record.work_item.as_deref()
                    || self.record.work_item.is_none()
                {
                    return Err(AgentError::NotAllowed(
                        "the conversation's work context belongs to another work item".into(),
                    ));
                }
                Ok(scope.clone())
            })
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

    pub fn model(&self) -> Option<&str> {
        self.record.model.as_deref()
    }

    /// Mark this conversation as made for background work: lists of the person's chats leave
    /// it out.
    pub fn set_background(&mut self, engine: &Engine) -> Result<(), AgentError> {
        self.record.background = true;
        self.save(engine)
    }

    pub fn set_model(&mut self, engine: &Engine, model: String) -> Result<(), AgentError> {
        self.record.model = Some(model);
        self.save(engine)
    }

    pub fn execution_mode(&self) -> ExecutionMode {
        self.record.execution_mode
    }

    pub fn execution_mode_revision(&self) -> u64 {
        self.record.execution_mode_revision
    }

    /// Change future-turn authority only when the caller saw the current revision.
    pub fn set_execution_mode(
        &mut self,
        engine: &Engine,
        expected_revision: u64,
        mode: ExecutionMode,
    ) -> Result<(), AgentError> {
        if self.record.execution_mode_revision != expected_revision {
            return Err(AgentError::StaleExecutionMode);
        }
        if self.record.execution_mode != mode {
            self.record.execution_mode = mode;
            self.record.execution_mode_revision =
                self.record.execution_mode_revision.saturating_add(1);
            self.record.updated_at = OffsetDateTime::now_utc();
            self.save(engine)?;
        }
        Ok(())
    }

    /// Read the posts that arrived since the last turn, for the next turn to carry.
    pub fn read_posts(&mut self, engine: &Engine) -> Result<(), AgentError> {
        self.unread = posts(
            engine,
            self.record.id.as_str(),
            self.record.posts_read,
            1000,
        )?;
        Ok(())
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
        if self.recent.is_empty() && self.record.gists.is_empty() && self.unread.is_empty() {
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
        if !self.unread.is_empty() {
            let _ = writeln!(
                s,
                "Posted in this chat since your last answer, by work it set up:"
            );
            let older = self.unread.len().saturating_sub(POSTS_CARRIED);
            if older > 0 {
                let _ = writeln!(s, "  (+{older} earlier posts)");
            }
            for (_, p) in &self.unread[older..] {
                let failed = if p.ok { "" } else { ", failed" };
                let text = clip(&p.text, self.record.policy.answer_chars);
                if p.kind == PostKind::Message {
                    let _ = writeln!(s, "  [you, later{failed}] {text}");
                } else {
                    let _ = writeln!(s, "  [{} · {}{failed}] {text}", p.from, p.title);
                }
            }
        }
        Some(s.trim_end().to_string())
    }

    /// The work name of turn `index` ([`Engine::for_work`]): its calls carry it.
    pub fn work_of(&self, index: u32) -> String {
        format!("session:{}/{index}", self.record.id)
    }

    /// The turns at the end of this conversation that did not finish (the app stopped, it
    /// failed, or the person stopped it), newest last, with the calls they made. The next turn
    /// continues from them: it does not make their changes again and is told what they did.
    /// `None` when the last turn finished.
    pub fn unfinished(&self, engine: &Engine) -> Result<Option<Unfinished>, AgentError> {
        let count = self.record.turn_count;
        // Read from the store, not the verbatim window: a chain may be longer than it.
        let stored = Self::read_turns(engine, &self.record.id, Some(UNFINISHED_TURNS + 1))?;
        let chain: Vec<Turn> = stored
            .into_iter()
            .filter(|t| t.index < count)
            .rev()
            .take_while(Turn::unfinished)
            .take(UNFINISHED_TURNS)
            .collect();
        let Some(last) = chain.first() else {
            return Ok(None);
        };
        let goal = last.effective_goal();
        let error = last.error.clone().or_else(|| {
            last.ran_out
                .then(|| "it reached its step limit before the work was done".to_string())
        });
        let stopped = last.cancelled && !last.incomplete;
        let mut turns: Vec<u32> = chain.iter().map(|t| t.index).collect();
        turns.reverse();
        let works: Vec<String> = turns.iter().map(|i| self.work_of(*i)).collect();
        let execution_project = self.execution_project(engine)?;
        let mut calls = Vec::new();
        for w in &works {
            calls.extend(engine.work_calls(&execution_project, w)?);
        }
        let lines = calls
            .iter()
            .map(|c| call_line(engine, &execution_project, c))
            .collect();
        Ok(Some(Unfinished {
            turns,
            works,
            calls,
            lines,
            goal,
            error,
            stopped,
        }))
    }

    /// The goal of a turn that starts now with `input`: the unfinished turn before it's goal
    /// with `input` added, or `None` when the turn before finished (the input is the goal).
    pub fn goal_for(&self, unfinished: Option<&Unfinished>, input: &str) -> Option<Goal> {
        unfinished.map(|u| {
            let mut g = u.goal.clone();
            g.then(input);
            g
        })
    }

    /// Record that a turn started, before it runs. If the process stops before
    /// [`record_turn`](Session::record_turn) or [`fail_turn`](Session::fail_turn), the next
    /// open marks the turn interrupted instead of losing it.
    pub fn begin_turn(&mut self, engine: &Engine, input: &str) -> Result<(), AgentError> {
        self.begin_turn_with_goal(engine, input, None)
    }

    /// [`begin_turn`](Session::begin_turn) for a turn that continues an unfinished one: its
    /// goal is stored with it at once, so a crash keeps it, and the turn's record keeps it
    /// whichever way it ends.
    pub fn begin_turn_with_goal(
        &mut self,
        engine: &Engine,
        input: &str,
        goal: Option<Goal>,
    ) -> Result<(), AgentError> {
        self.begin_turn_with(engine, input, goal, Vec::new())
    }

    /// [`begin_turn_with_goal`](Session::begin_turn_with_goal) for a message with attached
    /// images (project files): they are stored with the turn.
    pub fn begin_turn_with(
        &mut self,
        engine: &Engine,
        input: &str,
        goal: Option<Goal>,
        images: Vec<String>,
    ) -> Result<(), AgentError> {
        let mut t = Turn::placeholder(self.record.turn_count, input);
        t.execution_mode = self.record.execution_mode;
        t.goal = goal.clone();
        t.images = images.clone();
        self.goal = goal;
        self.images = images;
        self.steered.clear();
        self.turn_execution_mode = Some(t.execution_mode);
        engine.store.put_session_turn(
            self.record.id.as_str(),
            t.index,
            &serde_json::to_value(&t)?,
        )?;
        Ok(())
    }

    /// A message the person sent while the turn runs reached the model: add it to the turn's
    /// stored record now, so a crash keeps it in the goal the next turn continues.
    pub fn steered_now(&mut self, engine: &Engine, message: &str) -> Result<(), AgentError> {
        let index = self.record.turn_count;
        let Some(v) = engine
            .store
            .list_session_turns(self.record.id.as_str(), Some(1))?
            .into_iter()
            .next()
        else {
            return Ok(());
        };
        let mut t = readable_turn(v);
        if t.index != index || t.pid.is_none() {
            return Ok(());
        }
        t.steered.push(message.to_string());
        self.steered.push(message.to_string());
        engine.store.put_session_turn(
            self.record.id.as_str(),
            index,
            &serde_json::to_value(&t)?,
        )?;
        Ok(())
    }

    /// Store a finished turn and slide the carry-over window. A turn the process stopping cut
    /// short ([`HarnessOutcome::interrupted`]) is stored as interrupted, as after a crash, not
    /// as one the person stopped.
    pub fn record_turn(
        &mut self,
        engine: &Engine,
        input: &str,
        outcome: &HarnessOutcome,
    ) -> Result<(), AgentError> {
        let interrupted = outcome.interrupted;
        let turn = Turn {
            index: self.record.turn_count,
            input: input.to_string(),
            answer: outcome.answer.clone(),
            cited: outcome
                .cited
                .iter()
                .map(|c| c.short().to_string())
                .collect(),
            cancelled: outcome.cancelled && !interrupted,
            steered: outcome.steered.clone(),
            images: self.images.clone(),
            metrics: outcome.metrics.clone(),
            at: OffsetDateTime::now_utc(),
            incomplete: interrupted,
            error: interrupted.then(|| INTERRUPTED.to_string()),
            pid: None,
            pid_start: None,
            goal: self.goal.clone(),
            ran_out: outcome.ran_out && !outcome.cancelled,
            execution_mode: self
                .turn_execution_mode
                .unwrap_or(self.record.execution_mode),
            // `outcome.steered` is what reached the model; it holds what `steered_now` kept.
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
        t.execution_mode = self
            .turn_execution_mode
            .unwrap_or(self.record.execution_mode);
        t.error = Some(error.to_string());
        t.pid = None;
        t.pid_start = None;
        t.goal = self.goal.clone();
        // What the person said while it ran stays with it: the next turn's goal has it.
        t.steered = self.steered.clone();
        t.images = self.images.clone();
        self.push_turn(engine, t, &Metrics::default())
    }

    fn push_turn(
        &mut self,
        engine: &Engine,
        turn: Turn,
        metrics: &Metrics,
    ) -> Result<(), AgentError> {
        let mut next = self.clone();
        next.turn_execution_mode = None;
        next.goal = None;
        next.images.clear();
        if next.record.title.is_empty() {
            next.record.title = clip(&turn.input, 60);
        }
        next.record.turn_count += 1;
        if let Some((seq, _)) = next.unread.last() {
            next.record.posts_read = *seq;
            next.unread.clear();
        }
        next.record.usage.add(metrics);
        next.record.updated_at = OffsetDateTime::now_utc();
        next.recent.push(turn.clone());
        next.slide(
            next.record.policy.recent_turns,
            next.record.policy.max_gists,
        );
        let execution_project = self.execution_project(engine)?;
        let pending_id = format!("{}/{}", self.record.id, turn.index);
        let turn_value = serde_json::to_value(&turn)?;
        if self.record.work_item.is_some() && turn.pid.is_none() {
            engine
                .store
                .transact_records::<_, AgentError>(self.record.project.as_str(), |tx| {
                    match tx.get_record(PENDING_RESULT, &pending_id)? {
                        Some(existing) if existing != turn_value => {
                            return Err(std::io::Error::new(
                                std::io::ErrorKind::AlreadyExists,
                                "this conversation already has a different retained result",
                            )
                            .into())
                        }
                        Some(_) => {}
                        None => tx.insert_record(PENDING_RESULT, &pending_id, &turn_value)?,
                    }
                    Ok(())
                })?;
        }
        engine
            .store
            .transact_records::<_, AgentError>(self.record.project.as_str(), |tx| {
                let current = serde_json::from_value::<SessionRecord>(
                    tx.get_session(self.record.id.as_str())?.ok_or_else(|| {
                        std::io::Error::new(
                            std::io::ErrorKind::NotFound,
                            "conversation was deleted",
                        )
                    })?,
                )?
                .settled();
                if current.turn_count != turn.index {
                    let existing = tx.get_session_turn(self.record.id.as_str(), turn.index)?;
                    let same = existing.as_ref().is_some_and(|existing| {
                        existing["input"] == turn_value["input"]
                            && existing["answer"] == turn_value["answer"]
                            && existing["cancelled"] == turn_value["cancelled"]
                            && existing["error"] == turn_value["error"]
                            && existing["incomplete"] == turn_value["incomplete"]
                    });
                    if current.turn_count == turn.index.saturating_add(1) && same {
                        next.record = current;
                        if self.record.work_item.is_some() {
                            tx.delete_record(PENDING_RESULT, &pending_id)?;
                        }
                        return Ok(());
                    }
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::WouldBlock,
                        "the conversation changed while its answer was being saved",
                    )
                    .into());
                }
                if current.title != self.record.title {
                    next.record.title = current.title;
                }
                next.record.model = current.model;
                next.record.execution_mode = current.execution_mode;
                next.record.execution_mode_revision = current.execution_mode_revision;
                if self.record.execution_scope.is_some() && turn.pid.is_none() {
                    for invocation in tx.execution_invocations(
                        execution_project.as_str(),
                        &self.work_of(turn.index),
                    )? {
                        for id in invocation.produced {
                            let artifact = tx
                                .execution_artifact(execution_project.as_str(), id.as_str())?
                                .ok_or_else(|| AgentError::ArtifactNotFound(id.to_string()))?;
                            if artifact
                                .meta
                                .get("before")
                                .and_then(serde_json::Value::as_bool)
                                != Some(true)
                            {
                                tx.put_artifact(&artifact)?;
                            }
                        }
                    }
                }
                tx.put_session_turn(self.record.id.as_str(), turn.index, &turn_value)?;
                tx.put_session(
                    self.record.id.as_str(),
                    &serde_json::to_value(&next.record)?,
                )?;
                if self.record.work_item.is_some() {
                    tx.delete_record(PENDING_RESULT, &pending_id)?;
                }
                Ok(())
            })?;
        *self = next;
        Ok(())
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
            .get_research_state(&self.execution_project(engine)?, self.id().as_str())?
            .unwrap_or_default();
        report.state_before = estimate_tokens(&state.digest(crate::harness::STATE_DIGEST_ITEMS));
        report.state_items_removed = state.compact(6);
        report.state_after = estimate_tokens(&state.digest(crate::harness::STATE_DIGEST_ITEMS));
        engine.store.put_research_state(
            &self.execution_project(engine)?,
            self.id().as_str(),
            &state,
        )?;

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
        t.pid_start = None;
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

/// One line for a call of a stopped turn: its action, key input and the first line of its
/// result card (or its error), with `inv:` to read it again.
fn call_line(engine: &Engine, project: &ProjectId, c: &ark_core::Invocation) -> String {
    let mut line = c.action.clone();
    let input = crate::budget::key_input(&c.input);
    if !input.is_empty() {
        line.push(' ');
        line.push_str(&input);
    }
    let result = engine
        .store
        .get_evidence_card(project, c.id.as_str())
        .ok()
        .flatten()
        .map(|card| card.summary.lines().next().unwrap_or("").to_string())
        .or_else(|| c.error.clone())
        .unwrap_or_default();
    if !result.trim().is_empty() {
        line.push_str(" → ");
        line.push_str(&clip(&result, 100));
    }
    line.push_str(&format!(" [inv:{}]", c.id.short()));
    line
}

/// `s` clipped to `max` characters, keeping its line breaks.
fn clip_lines(s: &str, max: usize) -> String {
    let t = s.trim();
    if t.chars().count() <= max {
        t.to_string()
    } else {
        format!(
            "{}… (the rest is in the conversation)",
            t.chars().take(max).collect::<String>()
        )
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
    use std::sync::Arc;

    fn outcome(answer: &str) -> HarnessOutcome {
        HarnessOutcome {
            answer: answer.into(),
            cited: vec![],
            steps: 1,
            trivial: true,
            metrics: Metrics::start(),
            state: ResearchState::default(),
            cancelled: false,
            interrupted: false,
            steered: vec![],
            ran_out: false,
        }
    }

    #[test]
    fn work_result_publication_deduplicates_concurrent_and_legacy_posts() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Arc::new(Engine::open_with(dir.path(), Registry::new()).unwrap());
        let project = engine.create_project("p", "").unwrap();
        let session = Session::start(&engine, &project.id).unwrap();
        let id = session.id().to_string();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let results = std::thread::scope(|scope| {
            let mut calls = Vec::new();
            for _ in 0..2 {
                let engine = engine.clone();
                let id = id.clone();
                let barrier = barrier.clone();
                calls.push(scope.spawn(move || {
                    barrier.wait();
                    post_once(
                        &engine,
                        &id,
                        Post::new("agent", "Result", "retained answer", true).work("task-one"),
                    )
                    .unwrap()
                }));
            }
            calls
                .into_iter()
                .map(|call| call.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(results[0].0, results[1].0);
        assert_eq!(results.iter().filter(|(_, created)| *created).count(), 1);
        assert_eq!(posts(&engine, &id, 0, 10).unwrap().len(), 1);
        let retry = post_once(
            &engine,
            &id,
            Post::new("agent", "Result", "retained answer", true).work("task-one"),
        )
        .unwrap();
        assert!(!retry.1);
        let changed = post_once(
            &engine,
            &id,
            Post::new("agent", "Result", "new answer after retry", true).work("task-one"),
        )
        .unwrap();
        assert!(changed.1);
        let legacy =
            Post::new("agent", "Older result", "saved before receipts", true).work("legacy-task");
        let sequence = engine
            .store
            .append_event(&posts_stream(&id), &serde_json::to_value(&legacy).unwrap())
            .unwrap();
        let recovered = post_once(
            &engine,
            &id,
            Post::new("agent", "Older result", "saved before receipts", true).work("legacy-task"),
        )
        .unwrap();
        assert_eq!(recovered, (sequence, false));
        assert_eq!(posts(&engine, &id, 0, 10).unwrap().len(), 3);
    }

    #[test]
    fn named_session_creation_never_replaces_an_existing_chat() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::open_with(dir.path(), Registry::new()).unwrap();
        let project = engine.create_project("p", "").unwrap();
        let id = SessionId("stable-client-request".into());
        let mut first = Session::start_named(&engine, id.clone(), &project.id).unwrap();
        first
            .record_turn(&engine, "keep this", &outcome("kept"))
            .unwrap();

        assert!(Session::start_named(&engine, id.clone(), &project.id).is_err());
        let reopened = Session::open(&engine, id.as_str()).unwrap().unwrap();
        assert_eq!(reopened.record().turn_count, 1);
        assert_eq!(
            reopened.transcript(&engine, None).unwrap()[0].input,
            "keep this"
        );
    }

    #[test]
    fn selected_model_is_created_atomically_and_old_records_decode() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::open_with(dir.path(), Registry::new()).unwrap();
        let project = engine.create_project("p", "").unwrap();
        let id = SessionId("model-bound-client-request".into());
        let selected =
            Session::start_named_model(&engine, id.clone(), &project.id, "codex:gpt-5@high".into())
                .unwrap();
        assert_eq!(selected.model(), Some("codex:gpt-5@high"));
        assert_eq!(
            Session::open(&engine, id.as_str())
                .unwrap()
                .unwrap()
                .model(),
            Some("codex:gpt-5@high")
        );

        let mut legacy = serde_json::to_value(selected.record()).unwrap();
        legacy.as_object_mut().unwrap().remove("model");
        let legacy: SessionRecord = serde_json::from_value(legacy).unwrap();
        assert_eq!(legacy.model, None);
    }

    #[test]
    fn a_post_is_carried_once_by_the_next_turn_and_placed_after_the_last() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::open_with(dir.path(), Registry::new()).unwrap();
        let p = engine.create_project("p", "").unwrap();
        let mut s = Session::start(&engine, &p.id).unwrap();
        s.record_turn(
            &engine,
            "check the orders every morning",
            &outcome("Scheduled."),
        )
        .unwrap();
        let id = s.id().as_str().to_string();
        for i in 0..5 {
            post(
                &engine,
                &id,
                Post::new(
                    "orders",
                    "Morning orders",
                    format!("{i} new orders"),
                    i != 4,
                ),
            )
            .unwrap();
        }
        let all = posts(&engine, &id, 0, 100).unwrap();
        assert_eq!(all.len(), 5);
        assert!(all.iter().all(|(_, p)| p.after == Some(0)), "after turn 0");

        // Reopened, as a window would: the next turn carries the newest posts.
        let mut s = Session::open(&engine, &id).unwrap().unwrap();
        s.read_posts(&engine).unwrap();
        let digest = s.conversation_digest().unwrap();
        assert!(digest.contains("(+2 earlier posts)"), "{digest}");
        assert!(
            digest.contains("[orders · Morning orders] 3 new orders"),
            "{digest}"
        );
        assert!(
            digest.contains("[orders · Morning orders, failed] 4 new orders"),
            "{digest}"
        );
        assert!(
            !digest.contains("1 new orders"),
            "only the newest are carried"
        );
        s.record_turn(&engine, "thanks", &outcome("You're welcome."))
            .unwrap();
        s.read_posts(&engine).unwrap();
        assert!(
            !s.conversation_digest()
                .unwrap()
                .contains("Posted in this chat"),
            "a turn carries a post once"
        );
        let s = Session::open(&engine, &id).unwrap().unwrap();
        assert!(!s
            .conversation_digest()
            .unwrap()
            .contains("Posted in this chat"));
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
        let mut s = Session::start(&engine, &p.id).unwrap();
        engine
            .store
            .put_research_state(&p.id, s.id().as_str(), &st)
            .unwrap();
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

    fn chat() -> (tempfile::TempDir, Engine, Session) {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::open_with(dir.path(), Registry::new()).unwrap();
        let p = engine.create_project("p", "").unwrap();
        let s = Session::start(&engine, &p.id).unwrap();
        (dir, engine, s)
    }

    #[test]
    fn execution_mode_is_revisioned_persisted_and_frozen_on_the_turn() {
        let (_dir, engine, mut session) = chat();
        // Nobody has chosen: Auto, revision 0.
        assert_eq!(session.execution_mode(), ExecutionMode::Auto);
        assert_eq!(session.execution_mode_revision(), 0);
        session
            .set_execution_mode(&engine, 0, ExecutionMode::Manual)
            .unwrap();
        assert_eq!(session.execution_mode_revision(), 1);
        assert!(matches!(
            session.set_execution_mode(&engine, 0, ExecutionMode::Plan),
            Err(AgentError::StaleExecutionMode)
        ));
        session.begin_turn(&engine, "update the project").unwrap();
        session
            .set_execution_mode(&engine, 1, ExecutionMode::Plan)
            .unwrap();
        session
            .record_turn(&engine, "update the project", &outcome("done"))
            .unwrap();
        let turns = Session::read_turns(&engine, session.id(), None).unwrap();
        assert_eq!(turns[0].execution_mode, ExecutionMode::Manual);
        let reopened = Session::open(&engine, session.id().as_str())
            .unwrap()
            .unwrap();
        assert_eq!(reopened.execution_mode(), ExecutionMode::Plan);
        assert_eq!(reopened.execution_mode_revision(), 2);
    }

    #[test]
    fn a_chat_nobody_chose_a_mode_for_is_auto_and_a_choice_stays() {
        let (_dir, engine, mut session) = chat();
        // What an older version stored for a chat nobody chose: Manual, revision 0.
        session.record.execution_mode = ExecutionMode::Manual;
        session.save(&engine).unwrap();
        let id = session.id().as_str().to_string();
        let reopened = Session::open(&engine, &id).unwrap().unwrap();
        assert_eq!(reopened.execution_mode(), ExecutionMode::Auto);
        assert_eq!(reopened.execution_mode_revision(), 0);
        // A person asked for Manual: revision 1, kept.
        let mut chosen = reopened;
        chosen
            .set_execution_mode(&engine, 0, ExecutionMode::Manual)
            .unwrap();
        let again = Session::open(&engine, &id).unwrap().unwrap();
        assert_eq!(again.execution_mode(), ExecutionMode::Manual);
        assert_eq!(again.execution_mode_revision(), 1);
    }

    #[test]
    fn an_older_chat_stays_auto_after_its_first_message() {
        let (_dir, engine, mut session) = chat();
        // Stored by an older version: Manual, nobody chose.
        session.record.execution_mode = ExecutionMode::Manual;
        session.save(&engine).unwrap();
        let id = session.id().as_str().to_string();
        let mut open = Session::open(&engine, &id).unwrap().unwrap();
        assert_eq!(open.execution_mode(), ExecutionMode::Auto);
        open.begin_turn(&engine, "hello").unwrap();
        open.record_turn(&engine, "hello", &outcome("hi")).unwrap();
        // Recording a turn copies the stored mode back: it must not undo Auto.
        assert_eq!(open.execution_mode(), ExecutionMode::Auto);
        assert_eq!(open.execution_mode_revision(), 0);
        let again = Session::open(&engine, &id).unwrap().unwrap();
        assert_eq!(again.execution_mode(), ExecutionMode::Auto);
        let stored: SessionRecord =
            serde_json::from_value(engine.store.get_session(&id).unwrap().unwrap()).unwrap();
        assert_eq!(
            stored.execution_mode,
            ExecutionMode::Auto,
            "and it is stored so"
        );
    }

    /// A turn that fails, continuing whatever came before it.
    fn failed_turn(engine: &Engine, s: &mut Session, input: &str) {
        let u = s.unfinished(engine).unwrap();
        let goal = s.goal_for(u.as_ref(), input);
        s.begin_turn_with_goal(engine, input, goal).unwrap();
        s.fail_turn(engine, input, "the model's reply could not be used: x")
            .unwrap();
    }

    #[test]
    fn a_chain_keeps_the_request_and_every_instruction() {
        let (_d, engine, mut s) = chat();
        failed_turn(&engine, &mut s, "add syntax highlighting");
        failed_turn(&engine, &mut s, "use tree-sitter");
        let u = s.unfinished(&engine).unwrap().unwrap();
        let g = s.goal_for(Some(&u), "continue").unwrap();
        assert_eq!(g.request, "add syntax highlighting");
        assert_eq!(g.follow_ups, ["use tree-sitter", "continue"]);
        let text = g.render(4_000);
        assert!(
            text.contains("follow the newest where they differ"),
            "{text}"
        );
        assert_eq!(
            u.error.as_deref(),
            Some("the model's reply could not be used: x")
        );
    }

    #[test]
    fn a_long_chain_keeps_the_request_and_counts_older_messages() {
        let (_d, engine, mut s) = chat();
        failed_turn(&engine, &mut s, "the request");
        for i in 0..30 {
            failed_turn(&engine, &mut s, &format!("continue {i}"));
        }
        let u = s.unfinished(&engine).unwrap().unwrap();
        assert_eq!(
            u.goal.request, "the request",
            "the first request of any chain"
        );
        assert_eq!(u.goal.follow_ups.len(), 8);
        assert_eq!(u.goal.follow_ups.last().unwrap(), "continue 29");
        assert_eq!(u.goal.earlier, 22);
        assert!(u.goal.render(4_000).contains("(+22 earlier messages)"));
    }

    #[test]
    fn a_finished_turn_ends_the_chain() {
        let (_d, engine, mut s) = chat();
        failed_turn(&engine, &mut s, "first");
        s.begin_turn_with_goal(&engine, "continue", Some(Goal::new("first")))
            .unwrap();
        s.record_turn(&engine, "continue", &outcome("done"))
            .unwrap();
        assert!(s.unfinished(&engine).unwrap().is_none());
        assert!(s.goal_for(None, "something new").is_none());
    }

    #[test]
    fn the_goal_survives_every_way_a_turn_is_recorded() {
        let mut g = Goal::new("the request");
        g.then("continue");
        // Finished.
        let (_d, engine, mut s) = chat();
        s.begin_turn_with_goal(&engine, "continue", Some(g.clone()))
            .unwrap();
        s.record_turn(&engine, "continue", &outcome("ok")).unwrap();
        let t = Session::read_turns(&engine, s.id(), None).unwrap();
        assert_eq!(t[0].goal.as_ref(), Some(&g), "record_turn");
        // Failed.
        let (_d, engine, mut s) = chat();
        s.begin_turn_with_goal(&engine, "continue", Some(g.clone()))
            .unwrap();
        s.fail_turn(&engine, "continue", "boom").unwrap();
        let t = Session::read_turns(&engine, s.id(), None).unwrap();
        assert_eq!(t[0].goal.as_ref(), Some(&g), "fail_turn");
        // The process died: the store's next opening recovers the begun turn.
        let (_d, engine, mut s) = chat();
        s.begin_turn_with_goal(&engine, "continue", Some(g.clone()))
            .unwrap();
        s.steered_now(&engine, "and keep the theme").unwrap();
        let mut stored = Session::read_turns(&engine, s.id(), None).unwrap();
        let mut t = stored.remove(0);
        t.pid = Some(u32::MAX - 7);
        t.pid_start = Some(1);
        engine
            .store
            .put_session_turn(s.id().as_str(), t.index, &serde_json::to_value(&t).unwrap())
            .unwrap();
        let reopened = Session::open(&engine, s.id().as_str()).unwrap().unwrap();
        let t = Session::read_turns(&engine, reopened.id(), None).unwrap();
        assert_eq!(t[0].goal.as_ref(), Some(&g), "recovered on open");
        assert_eq!(
            t[0].steered,
            ["and keep the theme"],
            "steering kept at once"
        );
        let u = reopened.unfinished(&engine).unwrap().unwrap();
        assert_eq!(
            u.goal.follow_ups,
            ["continue", "and keep the theme"],
            "the next turn continues with what was said while it ran"
        );
    }

    #[test]
    fn a_fourth_continue_still_finds_the_first_turns_change() {
        let (_d, engine, mut s) = chat();
        let project = s.project().clone();
        failed_turn(&engine, &mut s, "save the chapter");
        let mut inv = ark_core::Invocation::new(
            "notes.save",
            ark_core::RiskTier::Mutate,
            serde_json::json!({"path": "ch1.md"}),
        );
        inv.status = ark_core::InvocationStatus::Ok;
        inv.work = Some(s.work_of(0));
        inv.narration = Some("Saving chapter one now.".into());
        engine.store.put_invocation(&project, &inv).unwrap();
        for _ in 0..4 {
            failed_turn(&engine, &mut s, "continue");
        }
        let u = s.unfinished(&engine).unwrap().unwrap();
        assert_eq!(u.turns.len(), 5);
        assert!(
            u.calls.iter().any(|c| c.id == inv.id),
            "turn 0's change is found, so it is not made again"
        );
        let carry = u.carry(&[], 6_000);
        assert!(carry.contains("done: notes.save path=ch1.md"), "{carry}");
        assert!(
            carry.contains("Your last note before it stopped: \"Saving chapter one now.\""),
            "{carry}"
        );
        assert!(
            carry.contains("It stopped because: the model's reply could not be used"),
            "{carry}"
        );
    }

    #[test]
    fn a_turn_that_ran_out_of_steps_is_continued() {
        let (_d, engine, mut s) = chat();
        s.begin_turn(&engine, "add syntax highlighting").unwrap();
        let mut o = outcome("Here is where I got to.");
        o.ran_out = true;
        s.record_turn(&engine, "add syntax highlighting", &o)
            .unwrap();
        let u = s
            .unfinished(&engine)
            .unwrap()
            .expect("its work is not done");
        let g = s.goal_for(Some(&u), "continue").unwrap();
        assert_eq!(g.request, "add syntax highlighting");
        assert!(u
            .carry(&[], 6_000)
            .contains("It stopped because: it reached its step limit"));
        // Its answer stays an answer for the person.
        let t = Session::read_turns(&engine, s.id(), None).unwrap();
        assert!(!t[0].incomplete && t[0].ran_out);
    }

    #[test]
    fn what_the_person_said_survives_a_turn_that_fails() {
        let (_d, engine, mut s) = chat();
        s.begin_turn(&engine, "add syntax highlighting").unwrap();
        s.steered_now(&engine, "use the theme's colours").unwrap();
        s.fail_turn(
            &engine,
            "add syntax highlighting",
            "the model's reply could not be used: x",
        )
        .unwrap();
        let u = s.unfinished(&engine).unwrap().unwrap();
        assert_eq!(u.goal.follow_ups, ["use the theme's colours"]);
        let g = s.goal_for(Some(&u), "continue").unwrap();
        assert_eq!(g.follow_ups, ["use the theme's colours", "continue"]);
    }

    #[test]
    fn a_new_request_after_a_stop_or_the_step_limit_is_what_the_model_does() {
        // Stopped by the person, then asked for something else.
        let (_d, engine, mut s) = chat();
        s.begin_turn(&engine, "add syntax highlighting").unwrap();
        let mut stopped = outcome("");
        stopped.cancelled = true;
        s.record_turn(&engine, "add syntax highlighting", &stopped)
            .unwrap();
        let u = s.unfinished(&engine).unwrap().unwrap();
        assert!(u.stopped);
        let g = s.goal_for(Some(&u), "thanks, now write a README").unwrap();
        let text = g.render(4_000);
        assert!(text.ends_with("If their newest message asks for something else, do that instead; the task above stays unfinished."), "{text}");
        assert!(text.contains("  - thanks, now write a README"), "{text}");
        assert!(u
            .carry(&[], 6_000)
            .contains("unless the person's newest message asks for something else"));
        // Out of steps, then asked for something else.
        let (_d, engine, mut s) = chat();
        s.begin_turn(&engine, "add syntax highlighting").unwrap();
        let mut o = outcome("Partly done.");
        o.ran_out = true;
        s.record_turn(&engine, "add syntax highlighting", &o)
            .unwrap();
        let u = s.unfinished(&engine).unwrap().unwrap();
        let text = s
            .goal_for(Some(&u), "write a README")
            .unwrap()
            .render(4_000);
        assert!(
            text.contains("  - write a README") && text.contains("do that instead"),
            "{text}"
        );
    }

    #[test]
    fn follow_ups_shown_shrink_with_the_budget() {
        let mut g = Goal::new("the request");
        for i in 0..8 {
            g.then(&format!("message {i} {}", "word ".repeat(100)));
        }
        let big = g.render(4_000);
        let small = g.render(1_000);
        assert_eq!(big.matches("\n  - message").count(), 8);
        assert_eq!(small.matches("\n  - message").count(), 2);
        assert!(small.contains("(+6 earlier messages)"));
        assert!(
            small.len() < big.len() / 3,
            "{} vs {}",
            small.len(),
            big.len()
        );
    }
}
