//! One person-visible view of work that runs on its own (ADR 0062): schedules, which start at a
//! time, and event automations, which start when a message arrives. Both front-ends show these
//! words; schedules keep their own view and calls unchanged.

use ark_orchestrate::{
    AutomationState, EventAutomationRecord, Orchestrator, TaskRecord, TaskState,
};
use serde::Serialize;

use crate::orchestration::{schedule_runs, schedules, RunView, ScheduleView};
use crate::report::{Report, Table};

/// Something that runs on its own, as a person sees it.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AutomationView {
    /// Starts at a time ([`ScheduleView`], unchanged).
    Time(ScheduleView),
    /// Starts when a message arrives.
    Event(EventView),
}

/// An event automation as both front-ends show it.
#[derive(Clone, Debug, Serialize)]
pub struct EventView {
    pub id: String,
    pub agent: String,
    pub objective: String,
    pub context: String,
    /// Which messages start it: `message from lara@example.com on gmail office@example.com`.
    pub says: String,
    pub source: String,
    pub account: String,
    pub from: Option<String>,
    /// `listening`, `full`, `paused`, `paused_failing`.
    pub state: AutomationState,
    /// Where it is, in words, what matters first.
    pub status: String,
    /// `live`, `ask`, `warn`, `err` or `muted`: how much it needs the person.
    pub tone: &'static str,
    /// What the person can do about it now: `resume`, `discard`, `check`, or none.
    pub recovery: Option<&'static str>,
    /// The last message that started it, in the source's words, and when.
    pub last_event: Option<String>,
    pub last_event_ms: Option<u64>,
    /// How the last run ended, in words.
    pub last: Option<String>,
    pub last_task: Option<String>,
    /// Messages waiting for the open run to end.
    pub pending: usize,
    pub runs: u32,
    pub matched: u32,
    pub chat: Option<String>,
    /// What each run may read and change, in words, as the person approved it.
    pub can_read: Vec<String>,
    pub can_change: Vec<String>,
}

/// Everything that runs on its own in `o`'s project, schedules first; only what reports to
/// `chat` when given.
pub fn automations(o: &Orchestrator, chat: Option<&str>) -> Vec<AutomationView> {
    let mut out: Vec<AutomationView> = schedules(o, chat)
        .into_iter()
        .map(AutomationView::Time)
        .collect();
    out.extend(
        o.automations()
            .iter()
            .filter(|a| chat.is_none() || a.task.report_to.as_deref() == chat)
            .map(|a| AutomationView::Event(event_view(o, a))),
    );
    out
}

pub fn event_view(o: &Orchestrator, a: &EventAutomationRecord) -> EventView {
    let last = a.last_task.as_deref().and_then(|t| o.task(t));
    let pending = o.automation_pending(&a.id).len();
    let (status, tone, recovery) = status(a, last.as_ref(), pending);
    let label = |ms: u64| ark_orchestrate::schedule::local_label(ms);
    let ended = last.as_ref().filter(|t| t.state.is_final()).and_then(|t| {
        let at = label(t.finished_ms.unwrap_or(t.created_ms));
        match t.state {
            TaskState::Done => Some(format!("ran {at}")),
            TaskState::Failed => Some(format!("did not finish {at}")),
            _ => None,
        }
    });
    let last_event = a.last_event.as_deref().and_then(|e| {
        let ev = o.event_log().event(e)?;
        Some(match ev.event.label.trim() {
            "" => format!("{} {}", ev.event.kind, label(ev.event.at_ms)),
            l => format!("{l}, {}", label(ev.event.at_ms)),
        })
    });
    EventView {
        id: a.id.clone(),
        agent: a.agent.clone(),
        objective: a.task.objective.clone(),
        context: a.task.context.clone(),
        says: a.pattern.describe(),
        source: a.pattern.source.clone(),
        account: a.pattern.account.clone(),
        from: a.pattern.actor.clone(),
        state: a.state,
        status,
        tone,
        recovery,
        last_event,
        last_event_ms: a.last_event_ms,
        last: ended,
        last_task: a.last_task.clone(),
        pending,
        runs: a.runs,
        matched: a.matched,
        chat: a.task.report_to.clone(),
        can_read: a.authority.paths.clone(),
        can_change: crate::labels::authority_words(&a.authority),
    }
}

/// The state in words, its tone and the recovery the person has.
fn status(
    a: &EventAutomationRecord,
    last: Option<&TaskRecord>,
    pending: usize,
) -> (String, &'static str, Option<&'static str>) {
    let label = |ms: u64| ark_orchestrate::schedule::local_label(ms);
    if let Some(t) = last.filter(|t| !t.state.is_final()) {
        if t.question.as_ref().is_some_and(|q| q.open()) {
            return ("waits for you".into(), "ask", None);
        }
        if let Some(at) = t.retry_at_ms {
            return (format!("trying again at {}", label(at)), "warn", None);
        }
        return ("running now".into(), "live", None);
    }
    match a.state {
        AutomationState::PausedFailing => {
            return (
                format!("paused: {} runs in a row did not finish", a.failures),
                "err",
                Some("resume"),
            )
        }
        AutomationState::Paused => return ("paused".into(), "muted", Some("resume")),
        AutomationState::Full => {
            return (
                format!("full: {pending} messages wait; new ones are not read"),
                "warn",
                Some("discard"),
            )
        }
        AutomationState::Listening => {}
    }
    if let Some(l) = a.limited.as_ref().filter(|l| l.until_ms > now_ms()) {
        return (
            format!("rate limited until {}", label(l.until_ms)),
            "warn",
            None,
        );
    }
    if let Some(t) = last {
        if t.effect_uncertain {
            return (
                "may or may not have sent: check before it runs again".into(),
                "err",
                Some("check"),
            );
        }
        if t.state == TaskState::Failed {
            let at = label(t.finished_ms.unwrap_or(t.created_ms));
            return (format!("did not finish {at}"), "err", None);
        }
    }
    let waiting = match pending {
        0 => String::new(),
        n => format!(", {n} waiting"),
    };
    (format!("listening{waiting}"), "muted", None)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// An event automation's runs, newest first, at most `limit`.
pub fn event_runs(o: &Orchestrator, id: &str, limit: usize) -> Vec<RunView> {
    o.automation_runs(id, limit)
        .into_iter()
        .map(|t| crate::orchestration::run_view(t, ark_orchestrate::schedule::local_label))
        .collect()
}

/// A schedule's or an event automation's runs.
pub fn runs(o: &Orchestrator, id: &str, limit: usize) -> Vec<RunView> {
    if id.starts_with("a-") {
        event_runs(o, id, limit)
    } else {
        schedule_runs(o, id, limit)
    }
}

/// What runs on its own in this project (`/automations`).
pub fn automations_report(o: &Orchestrator) -> Report {
    let r = Report::titled("Automations");
    let all = automations(o, None);
    if all.is_empty() {
        return r.note("Nothing runs on its own. Ask in words: \"every day at 2 AM, recount the stock\" or \"when Lara emails, reply using budget.csv\".");
    }
    let mut t = Table::with_header(&["id", "starts", "now", "runs", "task"]).right(3);
    for v in &all {
        let (id, starts, now, runs, task) = match v {
            AutomationView::Time(s) => (&s.id, s.says.clone(), &s.status, s.runs, &s.objective),
            AutomationView::Event(e) => (
                &e.id,
                format!("when {}", e.says),
                &e.status,
                e.runs,
                &e.objective,
            ),
        };
        t = t.row([
            id.clone(),
            starts,
            now.clone(),
            runs.to_string(),
            crate::fmt::clip(task, 60),
        ]);
    }
    r.table(t)
        .note("/automations pause|resume|stop|discard <id>, or ask in words.")
}
