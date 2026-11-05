//! Managed vendor sessions: the subscription runtime.
//!
//! A subscription CLI (Claude Code, Codex) keeps a conversation on its side and can continue
//! it. Sending the whole request again on every call wastes the account's capacity; keeping
//! one vendor conversation forever lets it grow without bound. A [`ManagedSession`] sits in
//! between, one per *lane* (one logical agent):
//!
//! - **Reuse and incremental turns.** While a vendor session is healthy, a call sends only
//!   what the vendor has not seen: new user and tool messages, tools it does not know yet,
//!   and changed instructions ([`Delta`]).
//! - **Context monitoring and rotation.** The vendor's context size is read from each
//!   reply's measured usage. Before a call would push it past
//!   [`SessionPolicy::max_context_tokens`] (or the turn limit), the session rotates: a new
//!   vendor session starts from ARK's own request, which the harness already keeps under its
//!   token budget. That request is the minimal rehydration; the vendor's old history is
//!   dropped.
//! - **Crash recovery.** Lane state is written to a [`SessionLedger`] before and after each
//!   call. A lane found mid-call after a restart, or one whose last call failed, never
//!   resumes: it rehydrates.
//! - **Idle cleanup and limits.** [`SubscriptionRuntime::sweep_idle`] releases sessions idle
//!   longer than [`SessionPolicy::idle_secs`], and an account never holds more open vendor
//!   sessions than its [`Capacity::max_sessions`] (the least recently used idle one goes).
//!
//! ARK's state stays canonical: working state, evidence and the turn record live in the ARK
//! store, so a vendor session can be thrown away at any time and the agent loses nothing.
//! Lanes never share a vendor session.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::capacity::{rate_limit_wait, Capacity, CapacitySnapshot, Limiter};
use crate::{
    estimate_tokens, Completion, Message, Provider, ProviderError, Result, Role, TextSink, ToolDef,
    WebHit,
};

/// What a resumed vendor session has not seen yet.
#[derive(Clone, Debug, Default)]
pub struct Delta {
    /// The application's instructions, when they changed since the vendor last saw them.
    pub system: Option<String>,
    /// The names of the tools available now, when the set changed.
    pub tools_now: Option<Vec<String>>,
    /// Definitions of tools the vendor has never been shown.
    pub new_tools: Vec<ToolDef>,
    /// New user and tool messages, in order. Assistant messages are never resent: the
    /// vendor has its own record of what it said.
    pub messages: Vec<Message>,
    /// Every tool available now, in full. Not sent as such: a backend that shapes the reply by
    /// the tools (Codex's reply schema, ADR 0049) reads it.
    pub tools: Vec<ToolDef>,
}

impl Delta {
    /// Estimated tokens this delta sends.
    pub fn tokens(&self) -> u32 {
        self.system.as_deref().map(estimate_tokens).unwrap_or(0)
            + self
                .tools_now
                .as_ref()
                .map(|t| estimate_tokens(&t.join(", ")))
                .unwrap_or(0)
            + self.new_tools.iter().map(tool_tokens).sum::<u32>()
            + self.messages.iter().map(message_tokens).sum::<u32>()
    }
}

/// What a vendor session was opened with. A backend that must restate it on every resume
/// (instead of relying on the vendor's own record) uses it.
#[derive(Clone, Copy)]
pub struct Base<'a> {
    pub system: &'a str,
    pub tools: &'a [ToolDef],
}

/// A model reached through a session the vendor keeps.
#[async_trait]
pub trait SessionBackend: Send + Sync {
    /// Native tools are disabled for every open, resume and stateless call.
    fn host_controls_tools(&self) -> bool {
        false
    }

    /// Human description, e.g. "claude sonnet".
    fn label(&self) -> String;
    /// The model identifier, e.g. "claude:sonnet".
    fn model(&self) -> String;
    /// Whether the vendor's model is sent [`Message::pictures`] ([`Provider::reads_pictures`]).
    fn reads_pictures(&self) -> bool {
        false
    }
    /// The vendor model's input window ([`Provider::context_window`]), when known.
    fn context_window(&self) -> Option<u32> {
        None
    }
    /// [`Provider::caches_system_prompt_whole`].
    fn caches_system_prompt_whole(&self) -> bool {
        false
    }
    /// The account whose capacity this backend draws on (`claude`, `codex`).
    fn account(&self) -> String;
    fn account_revision(&self) -> u64 {
        0
    }
    /// Start a vendor session from a full request. Returns the vendor's session id.
    async fn open(
        &self,
        system: &str,
        tools: &[ToolDef],
        messages: &[Message],
    ) -> Result<(String, Completion)>;
    /// Continue a vendor session with only what is new.
    async fn resume(&self, vendor: &str, base: Base<'_>, delta: &Delta) -> Result<Completion>;
    /// [`open`](SessionBackend::open), delivering answer text to `on_text` as it arrives. The
    /// default sends the whole text once the reply is complete.
    async fn open_streaming(
        &self,
        system: &str,
        tools: &[ToolDef],
        messages: &[Message],
        on_text: TextSink<'_>,
    ) -> Result<(String, Completion)> {
        let (vendor, c) = self.open(system, tools, messages).await?;
        if !c.content.is_empty() {
            on_text(&c.content);
        }
        Ok((vendor, c))
    }
    /// [`resume`](SessionBackend::resume), delivering answer text to `on_text` as it arrives.
    async fn resume_streaming(
        &self,
        vendor: &str,
        base: Base<'_>,
        delta: &Delta,
        on_text: TextSink<'_>,
    ) -> Result<Completion> {
        let c = self.resume(vendor, base, delta).await?;
        if !c.content.is_empty() {
            on_text(&c.content);
        }
        Ok(c)
    }
    /// Forget a vendor session ARK let go: delete what the vendor stored for it. Best effort;
    /// the default does nothing.
    async fn discard(&self, _vendor: &str) {}
    /// The vendor's own web search, outside any session ([`Provider::web_search`]).
    async fn web_search(&self, _query: &str, _limit: usize) -> Option<Result<Vec<WebHit>>> {
        None
    }
}

/// When a lane keeps, rotates or drops its vendor session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionPolicy {
    /// Keep vendor sessions between calls. Off: every call opens a new one (the old
    /// stateless behavior).
    pub reuse: bool,
    /// Rotate before a call would take the vendor's context past this many tokens.
    pub max_context_tokens: u32,
    /// Rotate after this many calls in one vendor session.
    pub max_turns: u32,
    /// Release a vendor session unused for this long.
    pub idle_secs: u64,
    /// Keep the vendor's own files of a session ARK let go (rotated, released, evicted).
    /// Off: the backend deletes them ([`SessionBackend::discard`]).
    #[serde(default)]
    pub keep_files: bool,
}

impl Default for SessionPolicy {
    fn default() -> Self {
        SessionPolicy {
            reuse: true,
            max_context_tokens: 64_000,
            max_turns: 100,
            idle_secs: 30 * 60,
            keep_files: false,
        }
    }
}

impl SessionPolicy {
    /// The default, overridden by `ARK_SESSION_REUSE` (`0` turns reuse off),
    /// `ARK_SESSION_MAX_CONTEXT`, `ARK_SESSION_MAX_TURNS`, `ARK_SESSION_IDLE_S` and
    /// `ARK_SESSION_KEEP_FILES` (`1` keeps the vendor's files).
    pub fn from_env() -> Self {
        let d = SessionPolicy::default();
        fn var<T: std::str::FromStr>(k: &str) -> Option<T> {
            std::env::var(k).ok().and_then(|v| v.trim().parse().ok())
        }
        SessionPolicy {
            reuse: std::env::var("ARK_SESSION_REUSE")
                .map(|v| v.trim() != "0")
                .unwrap_or(d.reuse),
            max_context_tokens: var("ARK_SESSION_MAX_CONTEXT").unwrap_or(d.max_context_tokens),
            max_turns: var("ARK_SESSION_MAX_TURNS").unwrap_or(d.max_turns),
            idle_secs: var("ARK_SESSION_IDLE_S").unwrap_or(d.idle_secs),
            keep_files: std::env::var("ARK_SESSION_KEEP_FILES")
                .map(|v| v.trim() == "1")
                .unwrap_or(d.keep_files),
        }
    }
}

/// Why a lane started a new vendor session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpenReason {
    /// The lane's first call.
    First,
    /// The next call would pass the context limit.
    ContextFull,
    /// The vendor session reached the turn limit.
    TurnLimit,
    /// It was idle too long and had been released.
    Idle,
    /// The previous call failed, so the vendor's state is unknown.
    AfterFailure,
    /// The previous call was cancelled before its reply was complete.
    Cancelled,
    /// The process stopped during the previous call.
    Recovered,
    /// The request held nothing the vendor had not seen (a repeat), so it starts clean.
    NothingNew,
    /// Released to make room for another session on the same account, or by request.
    Released,
    /// The policy turns reuse off.
    Disabled,
}

impl OpenReason {
    /// Whether this open replaces a vendor session that was in use.
    pub fn is_rotation(self) -> bool {
        matches!(
            self,
            OpenReason::ContextFull | OpenReason::TurnLimit | OpenReason::NothingNew
        )
    }
}

/// Something that happened to a lane's vendor session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionEventKind {
    /// A new vendor session started from ARK's request. `sent_tokens` is that request.
    Opened {
        reason: OpenReason,
        sent_tokens: u32,
    },
    /// The vendor session continued with a delta. `saved_tokens` is what resending the whole
    /// request would have cost on top.
    Resumed { sent_tokens: u32, saved_tokens: u32 },
    /// The call failed; the next one rehydrates.
    Failed { error: String },
    /// The vendor session was dropped (idle, evicted, released by the owner).
    Released { reason: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionEvent {
    pub lane: String,
    pub vendor: Option<String>,
    /// The vendor context after the event, in tokens (measured when the vendor reported it).
    pub context_tokens: u32,
    /// Unix time in milliseconds.
    pub at_ms: u64,
    #[serde(flatten)]
    pub kind: SessionEventKind,
}

/// A lane's state, for display.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    /// No vendor session (never opened, released, or dropped after a failure).
    Closed,
    /// A call is running.
    Busy,
    /// A vendor session is open and waiting.
    Idle,
}

/// One lane, for display.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub lane: String,
    pub model: String,
    pub account: String,
    pub vendor: Option<String>,
    pub state: SessionState,
    /// The vendor's context size after the last call.
    pub context_tokens: u32,
    pub max_context_tokens: u32,
    /// Calls in the current vendor session.
    pub turns: u32,
    /// Calls on this lane in total.
    pub calls: u64,
    /// Vendor sessions opened on this lane.
    pub opens: u64,
    /// Of those, opened to replace one in use (context or turn limit).
    pub rotations: u64,
    /// Estimated tokens sent to the vendor.
    pub sent_tokens: u64,
    /// Estimated tokens not sent because the vendor already had them.
    pub saved_tokens: u64,
    pub last_used_ms: u64,
    pub last_event: Option<SessionEvent>,
}

/// A lane's persisted state. The ledger keeps one per lane so a restarted process can resume
/// a healthy vendor session or rehydrate an unhealthy one.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LaneRecord {
    pub model: String,
    #[serde(default)]
    pub account: String,
    #[serde(default)]
    pub account_revision: u64,
    pub vendor: Option<String>,
    /// The instructions and tools the vendor session opened with.
    #[serde(default)]
    pub base_system: String,
    #[serde(default)]
    pub base_tools: Vec<ToolDef>,
    /// Digest of the instructions the vendor saw last.
    #[serde(default)]
    pub system_digest: u64,
    /// Tools whose definitions the vendor has seen.
    #[serde(default)]
    pub tools_known: Vec<String>,
    /// The tool set of the last call.
    #[serde(default)]
    pub tools_active: Vec<String>,
    /// Digests of user and tool messages the vendor has seen.
    #[serde(default)]
    pub seen: Vec<u64>,
    #[serde(default)]
    pub context_tokens: u32,
    #[serde(default)]
    pub turns: u32,
    /// Set while a call runs. Found set at load: the process stopped mid-call.
    #[serde(default)]
    pub in_flight: bool,
    /// Why the next open happens, when the vendor session was dropped.
    #[serde(default)]
    pub reopen: Option<OpenReason>,
    #[serde(default)]
    pub last_used_ms: u64,
    #[serde(default)]
    pub calls: u64,
    #[serde(default)]
    pub opens: u64,
    #[serde(default)]
    pub rotations: u64,
    #[serde(default)]
    pub sent_tokens: u64,
    #[serde(default)]
    pub saved_tokens: u64,
}

/// Where lane state survives a restart.
pub trait SessionLedger: Send + Sync {
    fn load(&self, lane: &str) -> Option<LaneRecord>;
    fn save(&self, lane: &str, record: &LaneRecord);
    fn remove(&self, lane: &str);
}

/// A ledger in memory: lanes survive within the process only.
#[derive(Default)]
pub struct MemoryLedger {
    lanes: Mutex<HashMap<String, LaneRecord>>,
}

impl SessionLedger for MemoryLedger {
    fn load(&self, lane: &str) -> Option<LaneRecord> {
        self.lanes.lock().unwrap().get(lane).cloned()
    }
    fn save(&self, lane: &str, record: &LaneRecord) {
        self.lanes
            .lock()
            .unwrap()
            .insert(lane.to_string(), record.clone());
    }
    fn remove(&self, lane: &str) {
        self.lanes.lock().unwrap().remove(lane);
    }
}

pub type SessionListener = Arc<dyn Fn(&SessionEvent) + Send + Sync>;

/// The sessions of one account, which share its limits.
struct Account {
    limiter: Arc<Limiter>,
    lanes: Mutex<Vec<Weak<ManagedSession>>>,
}

impl Account {
    /// Release least recently used idle sessions until a new one fits.
    async fn make_room(&self, opening: &ManagedSession) {
        let max = self.limiter.capacity().max_sessions.max(1);
        let lanes: Vec<Arc<ManagedSession>> = {
            let mut l = self.lanes.lock().unwrap();
            l.retain(|w| w.strong_count() > 0);
            l.iter()
                .filter_map(Weak::upgrade)
                .filter(|s| !std::ptr::eq(s.as_ref(), opening))
                .collect()
        };
        let mut open = 0;
        let mut idle: Vec<(u64, Arc<ManagedSession>)> = Vec::new();
        for s in lanes {
            let (is_open, busy, used) = {
                let st = s.state.lock().unwrap();
                (st.record.vendor.is_some(), st.busy, st.record.last_used_ms)
            };
            if is_open {
                open += 1;
                if !busy {
                    idle.push((used, s));
                }
            }
        }
        idle.sort_by_key(|(t, _)| *t);
        for (_, s) in idle {
            if open < max {
                break;
            }
            s.release("evicted: the account's session limit was reached")
                .await;
            open -= 1;
        }
    }
}

struct LaneState {
    record: LaneRecord,
    busy: bool,
    last_event: Option<SessionEvent>,
}

/// One lane's managed vendor session. It is a [`Provider`], so a harness uses it like any
/// other model; give each agent its own.
pub struct ManagedSession {
    lane: String,
    backend: Arc<dyn SessionBackend>,
    account: Arc<Account>,
    policy: SessionPolicy,
    ledger: Arc<dyn SessionLedger>,
    state: Mutex<LaneState>,
    /// One call at a time per lane: a vendor session is a single conversation.
    call: tokio::sync::Mutex<()>,
    listener: Mutex<Option<SessionListener>>,
}

enum Plan {
    Open(OpenReason),
    Resume(Delta),
}

impl ManagedSession {
    pub fn lane(&self) -> &str {
        &self.lane
    }

    /// Be told about every event on this lane (an orchestrator traces them).
    pub fn set_listener(&self, f: SessionListener) {
        *self.listener.lock().unwrap() = Some(f);
    }

    pub fn account_revision(&self) -> u64 {
        self.backend.account_revision()
    }

    pub fn snapshot(&self) -> SessionSnapshot {
        let st = self.state.lock().unwrap();
        let r = &st.record;
        SessionSnapshot {
            lane: self.lane.clone(),
            model: self.backend.model(),
            account: self.account.limiter.account().to_string(),
            vendor: r.vendor.clone(),
            state: if st.busy {
                SessionState::Busy
            } else if r.vendor.is_some() {
                SessionState::Idle
            } else {
                SessionState::Closed
            },
            context_tokens: r.context_tokens,
            max_context_tokens: self.policy.max_context_tokens,
            turns: r.turns,
            calls: r.calls,
            opens: r.opens,
            rotations: r.rotations,
            sent_tokens: r.sent_tokens,
            saved_tokens: r.saved_tokens,
            last_used_ms: r.last_used_ms,
            last_event: st.last_event.clone(),
        }
    }

    pub fn capacity(&self) -> CapacitySnapshot {
        self.account.limiter.snapshot()
    }

    /// Drop the vendor session; the next call rehydrates from ARK's request.
    pub async fn release(&self, reason: &str) {
        let vendor = {
            let mut st = self.state.lock().unwrap();
            let v = st.record.vendor.take();
            if v.is_some() {
                st.record.reopen = Some(OpenReason::Released);
                st.record.context_tokens = 0;
                st.record.turns = 0;
                st.record.seen.clear();
                self.ledger.save(&self.lane, &st.record);
            }
            v
        };
        if let Some(v) = vendor {
            self.emit(SessionEventKind::Released {
                reason: reason.to_string(),
            });
            if !self.policy.keep_files {
                self.backend.discard(&v).await;
            }
        }
    }

    /// Release the vendor session if it has been idle longer than the policy allows.
    async fn release_if_idle(&self, now_ms: u64) -> bool {
        let idle = {
            let st = self.state.lock().unwrap();
            st.record.vendor.is_some()
                && !st.busy
                && now_ms.saturating_sub(st.record.last_used_ms) > self.policy.idle_secs * 1000
        };
        if idle {
            self.release("idle").await;
            let mut st = self.state.lock().unwrap();
            st.record.reopen = Some(OpenReason::Idle);
            self.ledger.save(&self.lane, &st.record);
        }
        idle
    }

    fn emit(&self, kind: SessionEventKind) {
        let ev = {
            let mut st = self.state.lock().unwrap();
            let ev = SessionEvent {
                lane: self.lane.clone(),
                vendor: st.record.vendor.clone(),
                context_tokens: st.record.context_tokens,
                at_ms: now_ms(),
                kind,
            };
            st.last_event = Some(ev.clone());
            ev
        };
        let listener = self.listener.lock().unwrap().clone();
        if let Some(f) = listener {
            f(&ev);
        }
    }

    fn plan(&self, system: &str, messages: &[Message], tools: &[ToolDef]) -> Plan {
        let st = self.state.lock().unwrap();
        let r = &st.record;
        if !self.policy.reuse {
            return Plan::Open(OpenReason::Disabled);
        }
        if r.vendor.is_none() {
            return Plan::Open(r.reopen.unwrap_or(OpenReason::First));
        }
        if r.turns >= self.policy.max_turns {
            return Plan::Open(OpenReason::TurnLimit);
        }
        let seen: HashSet<u64> = r.seen.iter().copied().collect();
        let fresh: Vec<Message> = messages
            .iter()
            .filter(|m| m.role != Role::Assistant && !seen.contains(&message_digest(m)))
            .cloned()
            .collect();
        if fresh.is_empty() {
            return Plan::Open(OpenReason::NothingNew);
        }
        let names: Vec<String> = tools.iter().map(|t| t.name.clone()).collect();
        let known: HashSet<&str> = r.tools_known.iter().map(String::as_str).collect();
        let delta = Delta {
            system: (digest(system) != r.system_digest).then(|| system.to_string()),
            tools_now: (names != r.tools_active).then(|| names.clone()),
            new_tools: tools
                .iter()
                .filter(|t| !known.contains(t.name.as_str()))
                .cloned()
                .collect(),
            messages: fresh,
            tools: tools.to_vec(),
        };
        if r.context_tokens + delta.tokens() > self.policy.max_context_tokens {
            return Plan::Open(OpenReason::ContextFull);
        }
        Plan::Resume(delta)
    }

    async fn call(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDef],
        on_text: Option<TextSink<'_>>,
    ) -> Result<Completion> {
        let _one = self.call.lock().await;
        let plan = self.plan(system, messages, tools);
        if let Plan::Open(_) = plan {
            self.account.make_room(self).await;
        }
        let _slot = self.account.limiter.acquire().await;
        let full = request_tokens(system, messages, tools);
        let (old_vendor, base_system, base_tools) = {
            let mut st = self.state.lock().unwrap();
            st.busy = true;
            st.record.in_flight = true;
            self.ledger.save(&self.lane, &st.record);
            (
                st.record.vendor.clone(),
                st.record.base_system.clone(),
                st.record.base_tools.clone(),
            )
        };
        // Until the reply is complete, dropping this future (a cancelled turn) leaves the
        // vendor session in an unknown state: the guard then drops it.
        let guard = Unfinished {
            session: self,
            vendor: old_vendor.clone(),
            done: false,
        };
        let resumed = matches!(plan, Plan::Resume(_));
        let result = match &plan {
            Plan::Open(_) => {
                if let Some(v) = old_vendor.as_ref().filter(|_| !self.policy.keep_files) {
                    self.backend.discard(v).await;
                }
                match on_text {
                    Some(f) => {
                        self.backend
                            .open_streaming(system, tools, messages, f)
                            .await
                    }
                    None => self.backend.open(system, tools, messages).await,
                }
                .map(|(v, c)| (Some(v), c))
            }
            Plan::Resume(delta) => {
                let vendor = old_vendor.as_deref().unwrap_or_default();
                let base = Base {
                    system: &base_system,
                    tools: &base_tools,
                };
                match on_text {
                    Some(f) => self.backend.resume_streaming(vendor, base, delta, f).await,
                    None => self.backend.resume(vendor, base, delta).await,
                }
                .map(|c| (None, c))
            }
        };
        guard.finish();
        let event = {
            let mut st = self.state.lock().unwrap();
            st.busy = false;
            let r = &mut st.record;
            r.in_flight = false;
            r.last_used_ms = now_ms();
            match &result {
                Err(e) => {
                    r.vendor = None;
                    r.reopen = Some(OpenReason::AfterFailure);
                    r.context_tokens = 0;
                    r.turns = 0;
                    r.seen.clear();
                    if let Some(wait) = rate_limit_wait(e) {
                        self.account.limiter.cool_down(wait);
                    }
                    SessionEventKind::Failed {
                        error: e.to_string(),
                    }
                }
                Ok((vendor, c)) => {
                    r.calls += 1;
                    r.model = self.backend.model();
                    r.account = self.backend.account();
                    r.account_revision = self.backend.account_revision();
                    let event = match plan {
                        Plan::Open(reason) => {
                            r.vendor = vendor.clone();
                            r.reopen = None;
                            r.base_system = system.to_string();
                            r.base_tools = tools.to_vec();
                            r.tools_known = tools.iter().map(|t| t.name.clone()).collect();
                            r.seen.clear();
                            r.turns = 0;
                            r.opens += 1;
                            if reason.is_rotation() {
                                r.rotations += 1;
                            }
                            r.sent_tokens += full as u64;
                            SessionEventKind::Opened {
                                reason,
                                sent_tokens: full,
                            }
                        }
                        Plan::Resume(delta) => {
                            let sent = delta.tokens();
                            for t in &delta.new_tools {
                                r.tools_known.push(t.name.clone());
                            }
                            r.sent_tokens += sent as u64;
                            r.saved_tokens += full.saturating_sub(sent) as u64;
                            SessionEventKind::Resumed {
                                sent_tokens: sent,
                                saved_tokens: full.saturating_sub(sent),
                            }
                        }
                    };
                    r.system_digest = digest(system);
                    r.tools_active = tools.iter().map(|t| t.name.clone()).collect();
                    for m in messages.iter().filter(|m| m.role != Role::Assistant) {
                        let d = message_digest(m);
                        if !r.seen.contains(&d) {
                            r.seen.push(d);
                        }
                    }
                    r.turns += 1;
                    r.context_tokens = if c.usage.measured && c.usage.prompt_tokens > 0 {
                        c.usage.prompt_tokens + c.usage.completion_tokens
                    } else {
                        let sent = match &event {
                            SessionEventKind::Resumed { sent_tokens, .. } => *sent_tokens,
                            _ => 0,
                        };
                        let base = if matches!(event, SessionEventKind::Opened { .. }) {
                            full
                        } else {
                            r.context_tokens + sent
                        };
                        base + estimate_tokens(&c.content)
                    };
                    event
                }
            }
        };
        {
            let st = self.state.lock().unwrap();
            self.ledger.save(&self.lane, &st.record);
        }
        // A vendor session that failed is never resumed (the next call opens a new one): its
        // files go now, unless they are kept.
        if let (Err(_), true, Some(v)) = (&result, resumed, &old_vendor) {
            if !self.policy.keep_files {
                self.backend.discard(v).await;
            }
        }
        self.emit(event);
        result.map(|(_, c)| c)
    }
}

#[async_trait]
impl Provider for ManagedSession {
    fn host_controls_tools(&self) -> bool {
        self.backend.host_controls_tools()
    }
    fn connection_revision(&self) -> u64 {
        self.backend.account_revision()
    }
    async fn complete(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> Result<Completion> {
        self.call(system, messages, tools, None).await
    }

    async fn complete_streaming(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDef],
        on_text: TextSink<'_>,
    ) -> Result<Completion> {
        self.call(system, messages, tools, Some(on_text)).await
    }

    async fn web_search(&self, query: &str, limit: usize) -> Option<Result<Vec<WebHit>>> {
        self.backend.web_search(query, limit).await
    }
    fn reads_pictures(&self) -> bool {
        self.backend.reads_pictures()
    }
    fn caches_system_prompt_whole(&self) -> bool {
        self.backend.caches_system_prompt_whole()
    }

    /// The vendor thread is billed whole on every call, and it rotates at
    /// [`SessionPolicy::max_context_tokens`]: that is the window a request should be sized for.
    fn context_window(&self) -> Option<u32> {
        let w = self.backend.context_window();
        if !self.policy.reuse {
            return w;
        }
        w.map(|w| w.min(self.policy.max_context_tokens))
    }

    fn describe(&self) -> String {
        self.backend.label()
    }

    fn model_id(&self) -> String {
        self.backend.model()
    }
}

/// Held while a lane's call waits for the vendor. Dropped without [`finish`], the call was
/// cancelled: the vendor session is dropped (and discarded off this task, since a drop cannot
/// wait), so the next call opens a new one instead of resuming a reply cut off halfway.
///
/// [`finish`]: Unfinished::finish
struct Unfinished<'a> {
    session: &'a ManagedSession,
    vendor: Option<String>,
    done: bool,
}

impl Unfinished<'_> {
    fn finish(mut self) {
        self.done = true;
    }
}

impl Drop for Unfinished<'_> {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        let s = self.session;
        {
            let mut st = s.state.lock().unwrap();
            st.busy = false;
            let r = &mut st.record;
            r.in_flight = false;
            r.vendor = None;
            r.reopen = Some(OpenReason::Cancelled);
            r.context_tokens = 0;
            r.turns = 0;
            r.seen.clear();
            r.last_used_ms = now_ms();
            s.ledger.save(&s.lane, &st.record);
        }
        s.emit(SessionEventKind::Released {
            reason: "cancelled".into(),
        });
        if let (Some(v), false) = (self.vendor.take(), s.policy.keep_files) {
            let backend = s.backend.clone();
            if let Ok(rt) = tokio::runtime::Handle::try_current() {
                rt.spawn(async move { backend.discard(&v).await });
            }
        }
    }
}

/// Builds the backend for a model spec (`claude:sonnet`). `None`: not a session backend.
pub type BackendFactory = Arc<dyn Fn(&str, &str) -> Option<Arc<dyn SessionBackend>> + Send + Sync>;

/// The subscription runtime: the managed vendor sessions of every lane, grouped by account
/// so they share its [`Capacity`].
#[derive(Clone)]
pub struct SubscriptionRuntime {
    inner: Arc<RuntimeInner>,
}

struct RuntimeInner {
    policy: SessionPolicy,
    ledger: Arc<dyn SessionLedger>,
    capacities: Mutex<HashMap<String, Capacity>>,
    accounts: Mutex<HashMap<String, Arc<Account>>>,
    lanes: Mutex<HashMap<String, Arc<ManagedSession>>>,
    factory: BackendFactory,
}

impl Default for SubscriptionRuntime {
    fn default() -> Self {
        SubscriptionRuntime::builder().build()
    }
}

/// Builder for [`SubscriptionRuntime`].
pub struct SubscriptionRuntimeBuilder {
    policy: SessionPolicy,
    ledger: Arc<dyn SessionLedger>,
    capacities: HashMap<String, Capacity>,
    factory: BackendFactory,
}

impl SubscriptionRuntimeBuilder {
    pub fn policy(mut self, p: SessionPolicy) -> Self {
        self.policy = p;
        self
    }
    pub fn ledger(mut self, l: Arc<dyn SessionLedger>) -> Self {
        self.ledger = l;
        self
    }
    /// Limits for one account (default: [`Capacity::for_provider`]).
    pub fn capacity(mut self, account: impl Into<String>, c: Capacity) -> Self {
        self.capacities.insert(account.into(), c);
        self
    }
    /// How a model spec becomes a backend (default: the vendor CLIs, see
    /// [`crate::cli::session_backend`]).
    pub fn backends(mut self, f: BackendFactory) -> Self {
        self.factory = f;
        self
    }
    pub fn build(self) -> SubscriptionRuntime {
        SubscriptionRuntime {
            inner: Arc::new(RuntimeInner {
                policy: self.policy,
                ledger: self.ledger,
                capacities: Mutex::new(self.capacities),
                accounts: Mutex::new(HashMap::new()),
                lanes: Mutex::new(HashMap::new()),
                factory: self.factory,
            }),
        }
    }
}

impl SubscriptionRuntime {
    pub fn builder() -> SubscriptionRuntimeBuilder {
        SubscriptionRuntimeBuilder {
            policy: SessionPolicy::from_env(),
            ledger: Arc::new(MemoryLedger::default()),
            capacities: HashMap::new(),
            factory: Arc::new(crate::cli::session_backend),
        }
    }

    pub fn policy(&self) -> &SessionPolicy {
        &self.inner.policy
    }

    /// Whether `spec` (`claude:sonnet`, `codex`) names a backend this runtime manages.
    pub fn manages(&self, spec: &str) -> bool {
        let (p, m) = split_spec(spec);
        (self.inner.factory)(p, m).is_some()
    }

    /// Set an account's limits on a runtime already built. It takes effect only before the
    /// account's first use; returns false when the account is already in use.
    pub fn set_capacity(&self, account: &str, c: Capacity) -> bool {
        let accounts = self.inner.accounts.lock().unwrap();
        if accounts.contains_key(account) {
            return accounts[account].limiter.capacity() == c;
        }
        self.inner
            .capacities
            .lock()
            .unwrap()
            .insert(account.to_string(), c);
        true
    }

    /// The limiter of an account, created on first use. Stateless calls to the same account
    /// (a planner on the same subscription) should go through it too, see
    /// [`crate::capacity::Limited`].
    pub fn limiter(&self, account: &str) -> Arc<Limiter> {
        self.account(account).limiter.clone()
    }

    fn account(&self, name: &str) -> Arc<Account> {
        let mut accounts = self.inner.accounts.lock().unwrap();
        accounts
            .entry(name.to_string())
            .or_insert_with(|| {
                let cap = self
                    .inner
                    .capacities
                    .lock()
                    .unwrap()
                    .get(name)
                    .copied()
                    .unwrap_or_else(|| Capacity::for_provider(name));
                Arc::new(Account {
                    limiter: Arc::new(Limiter::new(name, cap)),
                    lanes: Mutex::new(Vec::new()),
                })
            })
            .clone()
    }

    /// The managed session of `lane` on model `spec`. The same lane and spec give the same
    /// session; a lane asked for a different spec gets a new session (the old one is
    /// released). `None` when `spec` is not a session backend.
    pub async fn session(&self, lane: &str, spec: &str) -> Option<Arc<ManagedSession>> {
        let (p, m) = split_spec(spec);
        let backend = (self.inner.factory)(p, m)?;
        Some(self.session_with(lane, backend).await)
    }

    /// [`session`](Self::session) with an explicit backend (tests, custom vendors).
    pub async fn session_with(
        &self,
        lane: &str,
        backend: Arc<dyn SessionBackend>,
    ) -> Arc<ManagedSession> {
        let existing = self.inner.lanes.lock().unwrap().get(lane).cloned();
        if let Some(s) = existing {
            if s.backend.model() == backend.model()
                && s.backend.account() == backend.account()
                && s.backend.account_revision() == backend.account_revision()
            {
                return s;
            }
            s.release("the lane moved to another model").await;
        }
        let account = self.account(&backend.account());
        let mut record = self.inner.ledger.load(lane).unwrap_or_default();
        if record.in_flight {
            // The process stopped during a call: what the vendor holds is unknown.
            record.vendor = None;
            record.in_flight = false;
            record.reopen = Some(OpenReason::Recovered);
            record.seen.clear();
            record.context_tokens = 0;
            record.turns = 0;
        }
        if !record.model.is_empty()
            && (record.model != backend.model()
                || (!record.account.is_empty() && record.account != backend.account())
                || record.account_revision != backend.account_revision())
        {
            record = LaneRecord::default();
        }
        let s = Arc::new(ManagedSession {
            lane: lane.to_string(),
            backend,
            account: account.clone(),
            policy: self.inner.policy.clone(),
            ledger: self.inner.ledger.clone(),
            state: Mutex::new(LaneState {
                record,
                busy: false,
                last_event: None,
            }),
            call: tokio::sync::Mutex::new(()),
            listener: Mutex::new(None),
        });
        account.lanes.lock().unwrap().push(Arc::downgrade(&s));
        self.inner
            .lanes
            .lock()
            .unwrap()
            .insert(lane.to_string(), s.clone());
        s
    }

    /// Every lane, for display.
    pub fn sessions(&self) -> Vec<SessionSnapshot> {
        let lanes: Vec<Arc<ManagedSession>> =
            self.inner.lanes.lock().unwrap().values().cloned().collect();
        let mut v: Vec<SessionSnapshot> = lanes.iter().map(|s| s.snapshot()).collect();
        v.sort_by(|a, b| a.lane.cmp(&b.lane));
        v
    }

    /// A lane's state as the ledger keeps it, for a lane this process has not used yet (an
    /// agent loaded after a restart). `None` when the ledger has nothing for it.
    pub fn stored(&self, lane: &str) -> Option<SessionSnapshot> {
        if let Some(s) = self.inner.lanes.lock().unwrap().get(lane) {
            return Some(s.snapshot());
        }
        let r = self.inner.ledger.load(lane)?;
        Some(SessionSnapshot {
            lane: lane.to_string(),
            account: split_spec(&r.model).0.to_string(),
            model: r.model.clone(),
            state: if r.vendor.is_some() && !r.in_flight {
                SessionState::Idle
            } else {
                SessionState::Closed
            },
            vendor: r.vendor.clone(),
            context_tokens: r.context_tokens,
            max_context_tokens: self.inner.policy.max_context_tokens,
            turns: r.turns,
            calls: r.calls,
            opens: r.opens,
            rotations: r.rotations,
            sent_tokens: r.sent_tokens,
            saved_tokens: r.saved_tokens,
            last_used_ms: r.last_used_ms,
            last_event: None,
        })
    }

    /// Every account's limiter, for display.
    pub fn capacity(&self) -> Vec<CapacitySnapshot> {
        let mut v: Vec<CapacitySnapshot> = self
            .inner
            .accounts
            .lock()
            .unwrap()
            .values()
            .map(|a| a.limiter.snapshot())
            .collect();
        v.sort_by(|a, b| a.account.cmp(&b.account));
        v
    }

    /// Release a lane's vendor session and forget the lane (an agent was terminated).
    pub async fn close(&self, lane: &str) {
        let s = self.inner.lanes.lock().unwrap().remove(lane);
        if let Some(s) = s {
            s.release("closed").await;
        }
        self.inner.ledger.remove(lane);
    }

    /// Release every vendor session idle longer than the policy allows. Returns how many.
    pub async fn sweep_idle(&self) -> usize {
        let lanes: Vec<Arc<ManagedSession>> =
            self.inner.lanes.lock().unwrap().values().cloned().collect();
        let now = now_ms();
        let mut n = 0;
        for s in lanes {
            if s.release_if_idle(now).await {
                n += 1;
            }
        }
        n
    }

    /// Run [`sweep_idle`](Self::sweep_idle) every `every` until the runtime is dropped.
    pub fn spawn_sweeper(&self, every: Duration) -> tokio::task::JoinHandle<()> {
        let weak = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(every).await;
                let Some(inner) = weak.upgrade() else { break };
                SubscriptionRuntime { inner }.sweep_idle().await;
            }
        })
    }
}

impl SubscriptionRuntime {
    /// A stateless provider on the same account as `spec`, for calls that should not enter
    /// an agent's vendor session (a planner, a critic). The vendor CLI's own stateless mode
    /// when there is one; otherwise each call opens a vendor session and discards it.
    pub fn stateless(&self, spec: &str) -> Option<Arc<dyn Provider>> {
        let (p, m) = split_spec(spec);
        let backend = (self.inner.factory)(p, m)?;
        let limiter = self.limiter(&backend.account());
        let inner: Arc<dyn Provider> = match crate::build(p, (!m.is_empty()).then(|| m.to_string()))
        {
            Ok(direct) => direct,
            Err(_) => Arc::new(OneShot { backend }),
        };
        Some(Arc::new(crate::capacity::Limited::new(inner, limiter)))
    }
}

/// Each call opens a vendor session and discards it.
struct OneShot {
    backend: Arc<dyn SessionBackend>,
}

#[async_trait]
impl Provider for OneShot {
    fn host_controls_tools(&self) -> bool {
        self.backend.host_controls_tools()
    }
    fn connection_revision(&self) -> u64 {
        self.backend.account_revision()
    }
    fn reads_pictures(&self) -> bool {
        self.backend.reads_pictures()
    }
    fn caches_system_prompt_whole(&self) -> bool {
        self.backend.caches_system_prompt_whole()
    }
    fn context_window(&self) -> Option<u32> {
        self.backend.context_window()
    }
    async fn complete(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> Result<Completion> {
        let (vendor, c) = self.backend.open(system, tools, messages).await?;
        self.backend.discard(&vendor).await;
        Ok(c)
    }
    async fn complete_streaming(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDef],
        on_text: TextSink<'_>,
    ) -> Result<Completion> {
        let (vendor, c) = self
            .backend
            .open_streaming(system, tools, messages, on_text)
            .await?;
        self.backend.discard(&vendor).await;
        Ok(c)
    }
    async fn web_search(&self, query: &str, limit: usize) -> Option<Result<Vec<WebHit>>> {
        self.backend.web_search(query, limit).await
    }
    fn describe(&self) -> String {
        self.backend.label()
    }
    fn model_id(&self) -> String {
        self.backend.model()
    }
}

/// `provider:model` split; a bare name has no model.
pub fn split_spec(spec: &str) -> (&str, &str) {
    match spec.split_once(':') {
        Some((p, m)) => (p.trim(), m.trim()),
        None => (spec.trim(), ""),
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// FNV-1a: stable across processes and Rust versions, unlike the std hasher.
fn digest(s: &str) -> u64 {
    digest_bytes(s.as_bytes())
}

fn digest_bytes(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn message_digest(m: &Message) -> u64 {
    let role = match m.role {
        Role::System => "s",
        Role::User => "u",
        Role::Assistant => "a",
        Role::Tool => "t",
    };
    // A picture counts by its bytes' digest, so a new picture is a new message.
    let pictures: String = m
        .pictures
        .iter()
        .map(|p| format!("\u{1}{:x}", digest_bytes(&p.bytes)))
        .collect();
    digest(&format!(
        "{role}\u{1}{}\u{1}{}\u{1}{}{pictures}",
        m.tool_call_id.as_deref().unwrap_or(""),
        m.tool_name.as_deref().unwrap_or(""),
        m.content
    ))
}

fn message_tokens(m: &Message) -> u32 {
    estimate_tokens(&m.content) + 4 + m.pictures.len() as u32 * crate::PICTURE_TOKENS
}

fn tool_tokens(t: &ToolDef) -> u32 {
    estimate_tokens(&t.name)
        + estimate_tokens(&t.description)
        + estimate_tokens(&t.parameters.to_string())
}

/// What a stateless call with this request would send.
fn request_tokens(system: &str, messages: &[Message], tools: &[ToolDef]) -> u32 {
    estimate_tokens(system)
        + tools.iter().map(tool_tokens).sum::<u32>()
        + messages.iter().map(message_tokens).sum::<u32>()
}

/// A scriptable session backend for tests: replies in order and records what each call sent.
pub mod mock {
    use super::*;

    /// One recorded backend call.
    #[derive(Clone, Debug)]
    pub enum Call {
        Open {
            vendor: String,
            system: String,
            tools: Vec<String>,
            messages: Vec<Message>,
        },
        Resume {
            vendor: String,
            delta: Delta,
        },
        Discard {
            vendor: String,
        },
    }

    pub struct MockSessions {
        model: String,
        account: String,
        replies: Mutex<std::collections::VecDeque<Result<Completion>>>,
        calls: Mutex<Vec<Call>>,
        opened: std::sync::atomic::AtomicU64,
        hang: std::sync::atomic::AtomicBool,
    }

    impl MockSessions {
        pub fn new(model: &str, account: &str, replies: Vec<Completion>) -> Self {
            MockSessions {
                model: model.into(),
                account: account.into(),
                replies: Mutex::new(replies.into_iter().map(Ok).collect()),
                calls: Mutex::new(Vec::new()),
                opened: Default::default(),
                hang: Default::default(),
            }
        }
        /// The next open or resume never answers (a call to cancel).
        pub fn hang_next(&self) {
            self.hang.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        async fn wait_if_hung(&self) {
            if self.hang.swap(false, std::sync::atomic::Ordering::SeqCst) {
                std::future::pending::<()>().await;
            }
        }
        /// Queue a failure as the next reply.
        pub fn push_error(&self, e: ProviderError) {
            self.replies.lock().unwrap().push_back(Err(e));
        }
        pub fn push(&self, c: Completion) {
            self.replies.lock().unwrap().push_back(Ok(c));
        }
        pub fn calls(&self) -> Vec<Call> {
            self.calls.lock().unwrap().clone()
        }
        fn next(&self) -> Result<Completion> {
            self.replies.lock().unwrap().pop_front().unwrap_or_else(|| {
                Ok(Completion {
                    content: "(script exhausted)".into(),
                    ..Default::default()
                })
            })
        }
    }

    #[async_trait]
    impl SessionBackend for MockSessions {
        fn host_controls_tools(&self) -> bool {
            true
        }
        fn label(&self) -> String {
            format!("mock-session {}", self.model)
        }
        fn model(&self) -> String {
            self.model.clone()
        }
        fn account(&self) -> String {
            self.account.clone()
        }
        async fn open(
            &self,
            system: &str,
            tools: &[ToolDef],
            messages: &[Message],
        ) -> Result<(String, Completion)> {
            let n = self
                .opened
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let vendor = format!("{}-v{n}", self.model);
            self.calls.lock().unwrap().push(Call::Open {
                vendor: vendor.clone(),
                system: system.into(),
                tools: tools.iter().map(|t| t.name.clone()).collect(),
                messages: messages.to_vec(),
            });
            self.wait_if_hung().await;
            self.next().map(|c| (vendor, c))
        }
        async fn resume(&self, vendor: &str, _base: Base<'_>, delta: &Delta) -> Result<Completion> {
            self.calls.lock().unwrap().push(Call::Resume {
                vendor: vendor.into(),
                delta: delta.clone(),
            });
            self.wait_if_hung().await;
            self.next()
        }
        async fn discard(&self, vendor: &str) {
            self.calls.lock().unwrap().push(Call::Discard {
                vendor: vendor.into(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::mock::{Call, MockSessions};
    use super::*;
    use crate::{ToolCall, Usage};

    fn reply(prompt: u32) -> Completion {
        Completion {
            content: "ok".into(),
            tool_calls: vec![],
            usage: Usage::measured(prompt, 5),
        }
    }

    fn tool(name: &str) -> ToolDef {
        ToolDef {
            name: name.into(),
            description: "d".into(),
            parameters: serde_json::json!({"type":"object"}),
        }
    }

    fn runtime(policy: SessionPolicy) -> SubscriptionRuntime {
        SubscriptionRuntime::builder().policy(policy).build()
    }

    #[tokio::test]
    async fn same_model_on_another_account_never_reuses_a_vendor_session() {
        let ledger: Arc<dyn SessionLedger> = Arc::new(MemoryLedger::default());
        let first = Arc::new(MockSessions::new(
            "same-model",
            "account-a",
            vec![reply(10)],
        ));
        let second = Arc::new(MockSessions::new(
            "same-model",
            "account-b",
            vec![reply(10)],
        ));
        let rt = SubscriptionRuntime::builder()
            .ledger(ledger.clone())
            .build();
        let a = rt.session_with("same-lane", first).await;
        a.complete("", &[Message::user("first")], &[])
            .await
            .unwrap();
        let b = rt.session_with("same-lane", second.clone()).await;
        assert!(!Arc::ptr_eq(&a, &b));
        b.complete("", &[Message::user("second")], &[])
            .await
            .unwrap();
        assert!(matches!(second.calls().first(), Some(Call::Open { .. })));
        drop(rt);
        let restarted = SubscriptionRuntime::builder().ledger(ledger).build();
        let third = Arc::new(MockSessions::new(
            "same-model",
            "account-a",
            vec![reply(10)],
        ));
        let a = restarted.session_with("same-lane", third.clone()).await;
        a.complete("", &[Message::user("after restart")], &[])
            .await
            .unwrap();
        assert!(matches!(third.calls().first(), Some(Call::Open { .. })));
    }

    #[tokio::test]
    async fn second_call_sends_only_what_is_new() {
        let rt = runtime(SessionPolicy::default());
        let backend = Arc::new(MockSessions::new("m", "acct", vec![reply(100), reply(150)]));
        let s = rt.session_with("agent-a", backend.clone()).await;
        let call = ToolCall {
            id: "c1".into(),
            name: "t".into(),
            arguments: serde_json::json!({}),
        };
        let first = vec![Message::user("task"), Message::user("state 1")];
        s.complete("sys", &first, &[tool("t")]).await.unwrap();
        let second = vec![
            Message::user("task"),
            Message::assistant_tool_calls(vec![call.clone()]),
            Message::tool_result(&call, "result"),
            Message::user("state 2"),
        ];
        s.complete("sys", &second, &[tool("t"), tool("u")])
            .await
            .unwrap();
        let calls = backend.calls();
        assert!(matches!(&calls[0], Call::Open { messages, .. } if messages.len() == 2));
        match &calls[1] {
            Call::Resume { vendor, delta } => {
                assert_eq!(vendor, "m-v0");
                let texts: Vec<&str> = delta.messages.iter().map(|m| m.content.as_str()).collect();
                assert_eq!(texts, vec!["result", "state 2"], "no task, no assistant");
                assert!(delta.system.is_none());
                assert_eq!(delta.new_tools.len(), 1);
                assert_eq!(delta.tools_now.as_ref().unwrap().len(), 2);
            }
            c => panic!("expected a resume, got {c:?}"),
        }
        let snap = s.snapshot();
        assert_eq!(snap.opens, 1);
        assert_eq!(snap.calls, 2);
        assert!(snap.saved_tokens > 0);
        assert_eq!(snap.context_tokens, 155, "measured prompt plus reply");
        assert_eq!(snap.state, SessionState::Idle);
    }

    #[tokio::test]
    async fn a_cancelled_call_drops_the_vendor_session_and_the_next_one_opens() {
        let rt = runtime(SessionPolicy::default());
        let backend = Arc::new(MockSessions::new("m", "acct", vec![reply(100), reply(120)]));
        let s = rt.session_with("agent-c", backend.clone()).await;
        s.complete("sys", &[Message::user("one")], &[])
            .await
            .unwrap();

        // The turn is cancelled while the vendor is still answering: the future is dropped.
        backend.hang_next();
        let msgs = [Message::user("one"), Message::user("two")];
        let call = s.complete("sys", &msgs, &[]);
        assert!(tokio::time::timeout(Duration::from_millis(50), call)
            .await
            .is_err());
        let snap = s.snapshot();
        assert_eq!(snap.state, SessionState::Closed, "{snap:?}");
        assert!(snap.vendor.is_none());
        tokio::task::yield_now().await;
        assert!(backend
            .calls()
            .iter()
            .any(|c| matches!(c, Call::Discard { vendor } if vendor == "m-v0")));

        let shown = Mutex::new(String::new());
        let sink = |t: &str| shown.lock().unwrap().push_str(t);
        s.complete_streaming("sys", &[Message::user("three")], &[], &sink)
            .await
            .unwrap();
        assert_eq!(
            *shown.lock().unwrap(),
            "ok",
            "a session backend's text streams"
        );
        let calls = backend.calls();
        assert!(
            matches!(calls.last(), Some(Call::Open { .. })),
            "the next call opens a new vendor session: {calls:?}"
        );
        assert!(matches!(
            s.snapshot().last_event.map(|e| e.kind),
            Some(SessionEventKind::Opened {
                reason: OpenReason::Cancelled,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn rotates_before_the_context_limit_from_arks_request() {
        let policy = SessionPolicy {
            max_context_tokens: 1000,
            ..Default::default()
        };
        let rt = runtime(policy);
        let backend = Arc::new(MockSessions::new("m", "acct", vec![reply(990), reply(200)]));
        let s = rt.session_with("a", backend.clone()).await;
        s.complete("sys", &[Message::user("one")], &[])
            .await
            .unwrap();
        let big = "x".repeat(400);
        s.complete("sys", &[Message::user("one"), Message::user(big)], &[])
            .await
            .unwrap();
        let calls = backend.calls();
        assert!(matches!(&calls[1], Call::Discard { vendor } if vendor == "m-v0"));
        assert!(
            matches!(&calls[2], Call::Open { vendor, messages, .. } if vendor == "m-v1" && messages.len() == 2),
            "rehydrated from the request ARK assembled"
        );
        let snap = s.snapshot();
        assert_eq!(snap.rotations, 1);
        assert!(matches!(
            snap.last_event.unwrap().kind,
            SessionEventKind::Opened {
                reason: OpenReason::ContextFull,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn a_failed_call_rehydrates_next_time() {
        let rt = runtime(SessionPolicy::default());
        let backend = Arc::new(MockSessions::new("m", "acct", vec![reply(10)]));
        backend.push_error(ProviderError::Api {
            provider: "m".into(),
            detail: "boom".into(),
        });
        backend.push(reply(10));
        let s = rt.session_with("a", backend.clone()).await;
        s.complete("sys", &[Message::user("1")], &[]).await.unwrap();
        assert!(s
            .complete("sys", &[Message::user("1"), Message::user("2")], &[])
            .await
            .is_err());
        assert_eq!(s.snapshot().state, SessionState::Closed);
        s.complete("sys", &[Message::user("1"), Message::user("2")], &[])
            .await
            .unwrap();
        let calls = backend.calls();
        assert!(matches!(calls[1], Call::Resume { .. }));
        assert!(
            matches!(&calls[2], Call::Discard { .. }),
            "the failed vendor session is discarded: {calls:?}"
        );
        assert!(matches!(&calls[3], Call::Open { messages, .. } if messages.len() == 2));
        assert!(matches!(
            s.snapshot().last_event.unwrap().kind,
            SessionEventKind::Opened {
                reason: OpenReason::AfterFailure,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn a_crash_mid_call_never_resumes() {
        let ledger = Arc::new(MemoryLedger::default());
        ledger.save(
            "a",
            &LaneRecord {
                model: "m".into(),
                vendor: Some("old".into()),
                in_flight: true,
                ..Default::default()
            },
        );
        let rt = SubscriptionRuntime::builder()
            .ledger(ledger.clone())
            .build();
        let backend = Arc::new(MockSessions::new("m", "acct", vec![reply(10)]));
        let s = rt.session_with("a", backend.clone()).await;
        assert_eq!(s.snapshot().state, SessionState::Closed);
        s.complete("sys", &[Message::user("go")], &[])
            .await
            .unwrap();
        assert!(matches!(&backend.calls()[0], Call::Open { .. }));
        assert!(matches!(
            s.snapshot().last_event.unwrap().kind,
            SessionEventKind::Opened {
                reason: OpenReason::Recovered,
                ..
            }
        ));
        assert!(!ledger.load("a").unwrap().in_flight);
    }

    #[tokio::test]
    async fn a_healthy_lane_resumes_after_a_restart() {
        let ledger: Arc<dyn SessionLedger> = Arc::new(MemoryLedger::default());
        let backend = Arc::new(MockSessions::new("m", "acct", vec![reply(10), reply(20)]));
        {
            let rt = SubscriptionRuntime::builder()
                .ledger(ledger.clone())
                .build();
            let s = rt.session_with("a", backend.clone()).await;
            s.complete("sys", &[Message::user("1")], &[]).await.unwrap();
        }
        let rt = SubscriptionRuntime::builder().ledger(ledger).build();
        let s = rt.session_with("a", backend.clone()).await;
        s.complete("sys", &[Message::user("1"), Message::user("2")], &[])
            .await
            .unwrap();
        assert!(matches!(&backend.calls()[1], Call::Resume { vendor, .. } if vendor == "m-v0"));
    }

    #[tokio::test]
    async fn a_lane_not_yet_used_shows_its_stored_state() {
        let ledger: Arc<dyn SessionLedger> = Arc::new(MemoryLedger::default());
        let backend = Arc::new(MockSessions::new("m", "acct", vec![reply(10)]));
        {
            let rt = SubscriptionRuntime::builder()
                .ledger(ledger.clone())
                .build();
            let s = rt.session_with("a", backend.clone()).await;
            s.complete("sys", &[Message::user("1")], &[]).await.unwrap();
        }
        let rt = SubscriptionRuntime::builder().ledger(ledger).build();
        assert!(rt.sessions().is_empty(), "nothing live in this process");
        let snap = rt.stored("a").unwrap();
        assert_eq!(snap.state, SessionState::Idle);
        assert_eq!(snap.calls, 1);
        assert!(rt.stored("b").is_none());
    }

    #[tokio::test]
    async fn lanes_never_share_a_vendor_session() {
        let rt = runtime(SessionPolicy::default());
        let backend = Arc::new(MockSessions::new("m", "acct", vec![reply(10), reply(10)]));
        let a = rt.session_with("a", backend.clone()).await;
        let b = rt.session_with("b", backend.clone()).await;
        a.complete("sys", &[Message::user("a")], &[]).await.unwrap();
        b.complete("sys", &[Message::user("b")], &[]).await.unwrap();
        assert_ne!(a.snapshot().vendor, b.snapshot().vendor);
        assert_eq!(rt.sessions().len(), 2);
    }

    #[tokio::test]
    async fn the_session_limit_evicts_the_least_recently_used() {
        let rt = SubscriptionRuntime::builder()
            .capacity("acct", Capacity::concurrent(2).with_max_sessions(2))
            .build();
        let backend = Arc::new(MockSessions::new("m", "acct", vec![]));
        for _ in 0..3 {
            backend.push(reply(10));
        }
        let a = rt.session_with("a", backend.clone()).await;
        let b = rt.session_with("b", backend.clone()).await;
        let c = rt.session_with("c", backend.clone()).await;
        a.complete("s", &[Message::user("a")], &[]).await.unwrap();
        tokio::time::sleep(Duration::from_millis(3)).await;
        b.complete("s", &[Message::user("b")], &[]).await.unwrap();
        tokio::time::sleep(Duration::from_millis(3)).await;
        c.complete("s", &[Message::user("c")], &[]).await.unwrap();
        assert_eq!(a.snapshot().state, SessionState::Closed, "oldest released");
        assert_eq!(b.snapshot().state, SessionState::Idle);
        assert_eq!(c.snapshot().state, SessionState::Idle);
    }

    #[tokio::test]
    async fn idle_sessions_are_swept() {
        let rt = runtime(SessionPolicy {
            idle_secs: 0,
            ..Default::default()
        });
        let backend = Arc::new(MockSessions::new("m", "acct", vec![reply(10), reply(10)]));
        let s = rt.session_with("a", backend.clone()).await;
        s.complete("s", &[Message::user("1")], &[]).await.unwrap();
        tokio::time::sleep(Duration::from_millis(5)).await;
        assert_eq!(rt.sweep_idle().await, 1);
        assert_eq!(s.snapshot().state, SessionState::Closed);
        s.complete("s", &[Message::user("1"), Message::user("2")], &[])
            .await
            .unwrap();
        assert!(matches!(
            s.snapshot().last_event.unwrap().kind,
            SessionEventKind::Opened {
                reason: OpenReason::Idle,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn reuse_off_opens_every_time() {
        let rt = runtime(SessionPolicy {
            reuse: false,
            ..Default::default()
        });
        let backend = Arc::new(MockSessions::new("m", "acct", vec![reply(10), reply(10)]));
        let s = rt.session_with("a", backend.clone()).await;
        s.complete("s", &[Message::user("1")], &[]).await.unwrap();
        s.complete("s", &[Message::user("1"), Message::user("2")], &[])
            .await
            .unwrap();
        let opens = backend
            .calls()
            .iter()
            .filter(|c| matches!(c, Call::Open { .. }))
            .count();
        assert_eq!(opens, 2);
    }

    #[tokio::test]
    async fn changed_instructions_travel_in_the_delta() {
        let rt = runtime(SessionPolicy::default());
        let backend = Arc::new(MockSessions::new("m", "acct", vec![reply(10), reply(10)]));
        let s = rt.session_with("a", backend.clone()).await;
        s.complete("v1", &[Message::user("1")], &[]).await.unwrap();
        s.complete("v2", &[Message::user("1"), Message::user("2")], &[])
            .await
            .unwrap();
        assert!(
            matches!(&backend.calls()[1], Call::Resume { delta, .. } if delta.system.as_deref() == Some("v2"))
        );
    }
}
