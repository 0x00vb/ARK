//! What happened in one turn, from the runtime's own events. A [`TurnRecord`] keeps the timed
//! event stream; [`interpret`] turns it into a [`Trace`]: a timeline grouped by step, and one
//! [`CallView`] per tool call with its approval, result, duration, and whether the result was
//! still in the model's context on later requests.
//!
//! Traces are built from structured events only, never from displayed text. `/trace` in the
//! terminal and the trace view in the GUI render the same [`Trace`].

use std::sync::{Arc, Mutex};
use std::time::Instant;

use ark_sdk::prelude::{ApprovalDecision, ContextReport, HarnessEvent};
use serde::{Deserialize, Serialize};

use crate::fmt::{clip, millis, tokens};
use crate::report::Tone;

/// One event and when it happened, in milliseconds since the turn began.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Timed {
    pub at_ms: u64,
    pub event: HarnessEvent,
}

/// One turn's input and event stream. Text deltas are merged per step to keep it compact.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TurnRecord {
    pub input: String,
    pub events: Vec<Timed>,
    pub elapsed_ms: u64,
    #[serde(default)]
    pub cancelled: bool,
    #[serde(default)]
    pub error: Option<String>,
    /// Still running.
    #[serde(default)]
    pub live: bool,
}

impl TurnRecord {
    pub fn push(&mut self, at_ms: u64, ev: &HarnessEvent) {
        if let HarnessEvent::TextDelta { text } = ev {
            if let Some(Timed {
                event: HarnessEvent::TextDelta { text: prev },
                ..
            }) = self.events.last_mut()
            {
                prev.push_str(text);
                return;
            }
        }
        self.events.push(Timed {
            at_ms,
            event: ev.clone(),
        });
    }
}

/// Records a running turn. Front-ends hold a [`SharedRecorder`] so they can show the trace of
/// a turn while it is still going.
#[derive(Debug)]
pub struct TurnRecorder {
    record: TurnRecord,
    started: Instant,
}

pub type SharedRecorder = Arc<Mutex<TurnRecorder>>;

impl Default for TurnRecorder {
    fn default() -> Self {
        TurnRecorder {
            record: TurnRecord::default(),
            started: Instant::now(),
        }
    }
}

impl TurnRecorder {
    pub fn shared() -> SharedRecorder {
        Arc::new(Mutex::new(TurnRecorder::default()))
    }
    pub(crate) fn start(&mut self, input: &str) {
        self.record = TurnRecord {
            input: input.to_string(),
            live: true,
            ..Default::default()
        };
        self.started = Instant::now();
    }
    pub fn record(&mut self, ev: &HarnessEvent) {
        let t = self.started.elapsed().as_millis() as u64;
        self.record.push(t, ev);
        self.record.elapsed_ms = t;
    }
    pub(crate) fn finish(&mut self, cancelled: bool, error: Option<String>) -> TurnRecord {
        self.record.elapsed_ms = self.started.elapsed().as_millis() as u64;
        self.record.cancelled = cancelled;
        self.record.error = error;
        self.record.live = false;
        self.record.clone()
    }
    /// The record so far.
    pub fn snapshot(&self) -> TurnRecord {
        let mut r = self.record.clone();
        if r.live {
            r.elapsed_ms = self.started.elapsed().as_millis() as u64;
        }
        r
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    Classified,
    Plan,
    Step,
    Context,
    ModelCall,
    Text,
    ToolCall,
    ToolResult,
    Reused,
    Retrieval,
    ApprovalAsked,
    Approval,
    Refused,
    Capability,
    State,
    Note,
    Present,
    SelfCheck,
    Steered,
    Retry,
    Stall,
    Cancelled,
    Answer,
}

/// How much of each turn's trace is stored (`ARK_TRACE`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TraceMode {
    /// Every event, as it happened (the default): a stored turn replays exactly.
    Full,
    /// Events without content: streamed text is dropped and tool inputs are emptied. Activity,
    /// timings, token reports and results stay.
    Summary,
    /// No trace is stored; the transcript still is.
    Off,
}

impl TraceMode {
    pub fn from_env() -> Self {
        match std::env::var("ARK_TRACE").as_deref() {
            Ok("summary") => TraceMode::Summary,
            Ok("off") => TraceMode::Off,
            _ => TraceMode::Full,
        }
    }
}

/// Stored traces per session; older turns keep their transcript entry but lose the trace.
pub const TRACES_KEPT: u32 = 1000;

/// The record as `mode` stores it, or `None` for nothing.
pub fn for_storage(record: &TurnRecord, mode: TraceMode) -> Option<TurnRecord> {
    match mode {
        TraceMode::Off => None,
        TraceMode::Full => Some(record.clone()),
        TraceMode::Summary => {
            let mut r = record.clone();
            r.events
                .retain(|t| !matches!(t.event, HarnessEvent::TextDelta { .. }));
            for t in &mut r.events {
                if let HarnessEvent::ToolCall { input, .. } = &mut t.event {
                    *input = serde_json::json!({});
                }
            }
            Some(r)
        }
    }
}

/// One line of the timeline.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TraceEntry {
    pub at_ms: u64,
    /// The step this entry belongs to (0 before the first step).
    pub step: usize,
    pub kind: EntryKind,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// The tool call number (1-based) this entry is about.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call: Option<usize>,
    #[serde(default)]
    pub tone: Tone,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallStatus {
    Running,
    Ok,
    Failed,
    /// The person said no.
    Denied,
    /// Needed approval and nobody could give it.
    Refused,
    /// An identical earlier result was used instead.
    Reused,
    /// The turn stopped before it ran.
    NotRun,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApprovalView {
    pub class: String,
    pub decision: ApprovalDecision,
    /// Covered by an earlier "allow for this session" (nobody was asked).
    pub remembered: bool,
}

/// One tool call, everything known about it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CallView {
    /// 1-based, in call order.
    pub n: usize,
    pub id: String,
    pub action: String,
    pub input: serde_json::Value,
    pub step: usize,
    pub at_ms: u64,
    pub status: CallStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    /// The stored invocation (full input and output).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<ApprovalView>,
    /// Estimated tokens the result added to the model's context (the distilled card).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_tokens: Option<u32>,
    /// Later requests in this turn that still carried the result.
    pub seen_by_requests: u32,
    /// The step whose request first left the result out (the window moved past it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left_window_at: Option<usize>,
}

/// A turn, interpreted.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Trace {
    pub input: String,
    pub elapsed_ms: u64,
    pub cancelled: bool,
    pub error: Option<String>,
    pub live: bool,
    pub steps: usize,
    pub model_calls: u32,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cached_tokens: u64,
    /// Whether every usage number came from the provider.
    pub measured: bool,
    /// The context report of each request, in order.
    pub requests: Vec<ContextReport>,
    pub entries: Vec<TraceEntry>,
    pub calls: Vec<CallView>,
}

impl Trace {
    pub fn call(&self, n: usize) -> Option<&CallView> {
        self.calls.iter().find(|c| c.n == n)
    }
}

/// The timeline line for one event: (kind, text, detail, tone). `None` for events a timeline
/// leaves out. Front-ends use it for their inline debug view too.
pub fn entry(ev: &HarnessEvent) -> Option<(EntryKind, String, Option<String>, Tone)> {
    use EntryKind as E;
    Some(match ev {
        HarnessEvent::Classified { trivial } => (
            E::Classified,
            if *trivial {
                "classified: direct".into()
            } else {
                "classified: investigation".into()
            },
            None,
            Tone::Muted,
        ),
        HarnessEvent::Plan { steps } => (
            E::Plan,
            format!("plan: {}", steps.join(" → ")),
            None,
            Tone::Normal,
        ),
        HarnessEvent::Step { index } => (E::Step, format!("step {index}"), None, Tone::Accent),
        HarnessEvent::Context { report } => (
            E::Context,
            format!(
                "context {} / {} · state {} · conversation {} · tool results {} · retrieved {}{}",
                tokens(report.total as u64),
                tokens(report.budget as u64),
                report.working_state,
                report.conversation,
                report.tool_rounds,
                report.retrieved,
                if report.compressed_rounds > 0 {
                    format!(" · {} rounds compressed", report.compressed_rounds)
                } else {
                    String::new()
                }
            ),
            None,
            Tone::Muted,
        ),
        HarnessEvent::ModelCall {
            role,
            model,
            usage,
            elapsed_ms,
            ..
        } => (
            E::ModelCall,
            format!(
                "model {role} · {model} · {} in / {} out{}{} · {}",
                usage.prompt_tokens,
                usage.completion_tokens,
                if usage.cached_tokens > 0 {
                    format!(" ({} cached)", usage.cached_tokens)
                } else {
                    String::new()
                },
                if usage.measured { "" } else { " (est.)" },
                millis(*elapsed_ms)
            ),
            None,
            Tone::Muted,
        ),
        HarnessEvent::TextDelta { text } => (
            E::Text,
            format!("text {} chars", text.chars().count()),
            Some(clip(text, 80)),
            Tone::Muted,
        ),
        HarnessEvent::ToolCall { action, input, .. } => (
            E::ToolCall,
            action.clone(),
            Some(clip(&input.to_string(), 120)),
            Tone::Normal,
        ),
        HarnessEvent::ToolResult {
            ok,
            summary,
            duration_ms,
            error,
            context_tokens,
            ..
        } => (
            E::ToolResult,
            format!(
                "{} {}{}",
                if *ok { "✓" } else { "✗" },
                clip(summary, 90),
                duration_ms.map(|d| format!(" · {d}ms")).unwrap_or_default()
            ),
            error
                .clone()
                .or_else(|| Some(format!("+{context_tokens} tokens to context"))),
            if *ok { Tone::Normal } else { Tone::Error },
        ),
        HarnessEvent::Duplicate { action, reused } => (
            E::Reused,
            format!("reused {action}"),
            Some(clip(reused, 100)),
            Tone::Muted,
        ),
        HarnessEvent::Retrieval { what } => {
            (E::Retrieval, format!("recall {what}"), None, Tone::Normal)
        }
        HarnessEvent::ApprovalRequested { request } => (
            E::ApprovalAsked,
            format!("asked: {}", request.preview),
            None,
            Tone::Warning,
        ),
        HarnessEvent::ApprovalResolved {
            decision, class, ..
        } => (
            E::Approval,
            format!("approval {} ({class})", decision_word(*decision)),
            None,
            if decision.allows() {
                Tone::Muted
            } else {
                Tone::Warning
            },
        ),
        HarnessEvent::NeedsApproval { action } => (
            E::Refused,
            format!("refused {action} (no approver)"),
            None,
            Tone::Warning,
        ),
        HarnessEvent::StateUpdate { kind, text } => (
            E::State,
            format!("state {kind}"),
            Some(clip(text, 100)),
            Tone::Normal,
        ),
        HarnessEvent::Note { kind, body } => (
            E::Note,
            format!("note {kind}"),
            Some(clip(body, 100)),
            Tone::Normal,
        ),
        HarnessEvent::Capability { action, detail } => (
            E::Capability,
            action.clone(),
            Some(clip(detail, 100)),
            Tone::Normal,
        ),
        HarnessEvent::Present { directive } => (
            E::Present,
            format!("present {}", directive.headline()),
            None,
            Tone::Normal,
        ),
        HarnessEvent::SelfCheck { passed, detail } => (
            E::SelfCheck,
            format!("self-check {}", if *passed { "passed" } else { "failed" }),
            Some(detail.clone()),
            if *passed { Tone::Muted } else { Tone::Warning },
        ),
        HarnessEvent::Steered { text } => (
            E::Steered,
            "your message reached the model".into(),
            Some(clip(text, 100)),
            Tone::Accent,
        ),
        HarnessEvent::Retry {
            role,
            attempt,
            delay_ms,
            error,
        } => (
            E::Retry,
            format!("retry {role} #{attempt} after {}", millis(*delay_ms)),
            Some(clip(error, 160)),
            Tone::Warning,
        ),
        HarnessEvent::Stall {
            idle_steps,
            stopped,
        } => (
            E::Stall,
            if *stopped {
                format!("stall: {idle_steps} idle steps, answer forced")
            } else {
                format!("stall: {idle_steps} idle steps, agent nudged")
            },
            None,
            Tone::Warning,
        ),
        HarnessEvent::Cancelled => (E::Cancelled, "cancelled".into(), None, Tone::Warning),
        HarnessEvent::Answer { text, streamed } => (
            E::Answer,
            format!(
                "answer ({} chars{})",
                text.chars().count(),
                if *streamed { ", streamed" } else { "" }
            ),
            None,
            Tone::Success,
        ),
    })
}

pub fn decision_word(d: ApprovalDecision) -> &'static str {
    match d {
        ApprovalDecision::AllowOnce => "allowed once",
        ApprovalDecision::AllowSession => "allowed for session",
        ApprovalDecision::Deny => "denied",
    }
}

/// Interpret a turn.
pub fn interpret(record: &TurnRecord) -> Trace {
    let mut t = Trace {
        input: record.input.clone(),
        elapsed_ms: record.elapsed_ms,
        cancelled: record.cancelled,
        error: record.error.clone(),
        live: record.live,
        steps: 0,
        model_calls: 0,
        prompt_tokens: 0,
        completion_tokens: 0,
        cached_tokens: 0,
        measured: true,
        requests: Vec::new(),
        entries: Vec::new(),
        calls: Vec::new(),
    };
    let mut step = 0usize;
    // Approval asked for the open call (so a resolution without a request was a remembered grant).
    let mut asked = false;
    for Timed { at_ms, event } in &record.events {
        let mut call_ref = None;
        match event {
            HarnessEvent::Step { index } => {
                step = *index;
                t.steps = t.steps.max(*index);
            }
            HarnessEvent::Context { report } => {
                // A call's round is index `call.step` in the working set; this request dropped
                // rounds below `compressed_rounds`.
                for c in t.calls.iter_mut().filter(|c| c.step < step) {
                    if !matches!(
                        c.status,
                        CallStatus::Ok | CallStatus::Failed | CallStatus::Reused
                    ) {
                        continue;
                    }
                    if c.step >= report.compressed_rounds {
                        c.seen_by_requests += 1;
                    } else if c.left_window_at.is_none() {
                        c.left_window_at = Some(step);
                    }
                }
                t.requests.push(report.clone());
            }
            HarnessEvent::ModelCall { usage, .. } => {
                t.model_calls += 1;
                t.prompt_tokens += usage.prompt_tokens as u64;
                t.completion_tokens += usage.completion_tokens as u64;
                t.cached_tokens += usage.cached_tokens as u64;
                t.measured &= usage.measured;
            }
            HarnessEvent::ToolCall { id, action, input } => {
                let n = t.calls.len() + 1;
                t.calls.push(CallView {
                    n,
                    id: id.clone(),
                    action: action.clone(),
                    input: input.clone(),
                    step,
                    at_ms: *at_ms,
                    status: CallStatus::Running,
                    summary: None,
                    error: None,
                    duration_ms: None,
                    invocation: None,
                    approval: None,
                    context_tokens: None,
                    seen_by_requests: 0,
                    left_window_at: None,
                });
                asked = false;
                call_ref = Some(n);
            }
            HarnessEvent::ToolResult {
                id,
                ok,
                summary,
                invocation,
                duration_ms,
                error,
                context_tokens,
                ..
            } => {
                if let Some(c) = t.calls.iter_mut().rev().find(|c| &c.id == id) {
                    c.status = if *ok {
                        CallStatus::Ok
                    } else {
                        CallStatus::Failed
                    };
                    c.summary = Some(summary.clone());
                    c.error = error.clone();
                    c.duration_ms = *duration_ms;
                    c.invocation = Some(invocation.clone());
                    c.context_tokens = Some(*context_tokens);
                    call_ref = Some(c.n);
                }
            }
            HarnessEvent::ApprovalRequested { .. } => asked = true,
            HarnessEvent::ApprovalResolved {
                action,
                class,
                decision,
            } => {
                if let Some(c) = open_call(&mut t.calls, action) {
                    c.approval = Some(ApprovalView {
                        class: class.clone(),
                        decision: *decision,
                        remembered: !asked,
                    });
                    if !decision.allows() {
                        c.status = CallStatus::Denied;
                    }
                    call_ref = Some(c.n);
                }
            }
            HarnessEvent::NeedsApproval { action } => {
                if let Some(c) = open_call(&mut t.calls, action) {
                    c.status = CallStatus::Refused;
                    call_ref = Some(c.n);
                }
            }
            HarnessEvent::Duplicate { action, reused } => {
                if let Some(c) = open_call(&mut t.calls, action) {
                    c.status = CallStatus::Reused;
                    c.summary = Some(reused.clone());
                    call_ref = Some(c.n);
                }
            }
            _ => {}
        }
        if let Some((kind, text, detail, tone)) = entry(event) {
            t.entries.push(TraceEntry {
                at_ms: *at_ms,
                step,
                kind,
                text,
                detail,
                call: call_ref,
                tone,
            });
        }
    }
    if !record.live {
        for c in t
            .calls
            .iter_mut()
            .filter(|c| c.status == CallStatus::Running)
        {
            c.status = CallStatus::NotRun;
        }
    }
    t
}

/// The most recent call to `action` still waiting for its outcome.
fn open_call<'a>(calls: &'a mut [CallView], action: &str) -> Option<&'a mut CallView> {
    calls
        .iter_mut()
        .rev()
        .find(|c| c.action == action && c.status == CallStatus::Running)
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_summary_trace_keeps_activity_but_no_content() {
        let mut r = TurnRecord::default();
        r.push(
            1,
            &HarnessEvent::ToolCall {
                id: "c".into(),
                action: "vault.read".into(),
                input: json!({ "key": "secret" }),
            },
        );
        r.push(
            2,
            &HarnessEvent::TextDelta {
                text: "the answer".into(),
            },
        );
        let s = for_storage(&r, TraceMode::Summary).unwrap();
        assert_eq!(s.events.len(), 1, "streamed text is dropped");
        match &s.events[0].event {
            HarnessEvent::ToolCall { action, input, .. } => {
                assert_eq!(action, "vault.read");
                assert_eq!(input, &json!({}));
            }
            other => panic!("{other:?}"),
        }
        assert!(for_storage(&r, TraceMode::Off).is_none());
        assert_eq!(for_storage(&r, TraceMode::Full).unwrap().events.len(), 2);
    }
    use super::*;
    use ark_sdk::prelude::ark_model::Usage;
    use serde_json::json;

    fn rec(events: Vec<HarnessEvent>) -> TurnRecord {
        let mut r = TurnRecord {
            input: "q".into(),
            ..Default::default()
        };
        for (i, e) in events.iter().enumerate() {
            r.push(i as u64 * 10, e);
        }
        r
    }

    fn ctx(compressed: usize) -> HarnessEvent {
        HarnessEvent::Context {
            report: ContextReport {
                compressed_rounds: compressed,
                total: 100,
                budget: 6000,
                ..Default::default()
            },
        }
    }

    fn call(id: &str, action: &str) -> HarnessEvent {
        HarnessEvent::ToolCall {
            id: id.into(),
            action: action.into(),
            input: json!({}),
        }
    }

    fn result(id: &str, action: &str) -> HarnessEvent {
        HarnessEvent::ToolResult {
            id: id.into(),
            action: action.into(),
            ok: true,
            summary: "done".into(),
            invocation: "inv1".into(),
            duration_ms: Some(4),
            error: None,
            context_tokens: 30,
            raw_tokens: 300,
        }
    }

    #[test]
    fn calls_track_approval_result_and_context_window() {
        let t = interpret(&rec(vec![
            HarnessEvent::Step { index: 1 },
            ctx(0),
            HarnessEvent::ModelCall {
                role: "reason".into(),
                model: "m".into(),
                usage: Usage::measured(100, 5),
                elapsed_ms: 20,
                first_token_ms: None,
                retries: 0,
            },
            call("c1", "notes.save"),
            HarnessEvent::ApprovalRequested {
                request: ark_sdk::prelude::ApprovalRequest {
                    action: "notes.save".into(),
                    summary: "save".into(),
                    risk: ark_sdk::prelude::RiskTier::Mutate,
                    input: json!({}),
                    class: "notes.save".into(),
                    preview: "notes.save()".into(),
                },
            },
            HarnessEvent::ApprovalResolved {
                action: "notes.save".into(),
                class: "notes.save".into(),
                decision: ApprovalDecision::AllowOnce,
            },
            result("c1", "notes.save"),
            call("c2", "notes.save"),
            HarnessEvent::ApprovalResolved {
                action: "notes.save".into(),
                class: "notes.save".into(),
                decision: ApprovalDecision::AllowSession,
            },
            call("c3", "notes.save"),
            HarnessEvent::ApprovalResolved {
                action: "notes.save".into(),
                class: "notes.save".into(),
                decision: ApprovalDecision::Deny,
            },
            HarnessEvent::Step { index: 2 },
            ctx(0),
            HarnessEvent::Step { index: 3 },
            ctx(2),
            HarnessEvent::Cancelled,
        ]));
        assert_eq!(t.calls.len(), 3);
        let c1 = t.call(1).unwrap();
        assert_eq!(c1.status, CallStatus::Ok);
        assert!(!c1.approval.as_ref().unwrap().remembered);
        assert_eq!(c1.context_tokens, Some(30));
        assert_eq!(c1.seen_by_requests, 1, "step 2 carried it");
        assert_eq!(c1.left_window_at, Some(3), "step 3 dropped rounds 0 and 1");
        let c2 = t.call(2).unwrap();
        assert!(
            c2.approval.as_ref().unwrap().remembered,
            "no request event: an earlier session grant covered it"
        );
        assert_eq!(
            c2.status,
            CallStatus::NotRun,
            "the turn was cancelled before it ran"
        );
        assert_eq!(t.call(3).unwrap().status, CallStatus::Denied);
        assert_eq!(t.model_calls, 1);
        assert_eq!(t.steps, 3);
        assert_eq!(t.requests.len(), 3);
        assert!(t.entries.iter().any(|e| e.kind == EntryKind::Cancelled));
    }

    #[test]
    fn text_deltas_merge_and_unfinished_calls_did_not_run() {
        let mut r = rec(vec![
            HarnessEvent::TextDelta { text: "Hel".into() },
            HarnessEvent::TextDelta { text: "lo".into() },
            call("c1", "notes.read"),
        ]);
        r.cancelled = true;
        assert_eq!(r.events.len(), 2);
        let t = interpret(&r);
        assert_eq!(t.calls[0].status, CallStatus::NotRun);
        assert_eq!(t.entries[0].detail.as_deref(), Some("Hello"));
    }
}
