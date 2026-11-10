//! Orchestration views: the agent and task graph, and one task's trace, as [`Report`]s. The
//! terminal prints them; the window renders the same report or uses its typed data
//! (`view: "orchestration"` carries the whole [`Graph`], `view: "orchestration_task"` the
//! task and its events). Both come from the orchestrator's own records and event log. The
//! project's board (`/board`) is read and changed here too, for both front-ends.

use ark_orchestrate::board::{Board, PERSON};
use ark_orchestrate::{
    AgentState, EventKind, Graph, OrchEvent, Origin, TaskNode, TaskState, WorkChange, WorkItem,
    WorkState,
};
use ark_sdk::prelude::ark_model::SessionState;
use ark_sdk::prelude::ApprovalDecision;

use crate::fmt::{clip, count, millis, tokens};
use crate::labels::Labels;
use crate::report::{Cell, Report, Row, Table, Tone};

pub fn task_tone(s: TaskState) -> Tone {
    match s {
        TaskState::Done => Tone::Success,
        TaskState::Failed => Tone::Error,
        TaskState::Cancelled => Tone::Muted,
        TaskState::Running => Tone::Accent,
        TaskState::Waiting | TaskState::Queued | TaskState::Pending | TaskState::Suspended => {
            Tone::Warning
        }
    }
}

fn agent_word(s: AgentState) -> &'static str {
    match s {
        AgentState::Idle => "idle",
        AgentState::Running => "running",
        AgentState::Waiting => "waiting",
        AgentState::Suspended => "suspended",
        AgentState::Terminated => "terminated",
    }
}

/// What an agent is doing now, in one line, from the runtime's graph alone (ADR 0036): the
/// question it waits on, what its task waits for, or its current call in words. `None` when it
/// has no task. Every front-end shows this line; none works it out from events.
pub fn doing(g: &Graph, agent: &ark_orchestrate::AgentNode, labels: &Labels) -> Option<String> {
    // A task of its own that asks the person, else the first it runs (after a restart the
    // agent is free while a task waits for an answer).
    let running = |t: &&ark_orchestrate::TaskNode| agent.running.contains(&t.task);
    let task = g
        .tasks
        .iter()
        .filter(running)
        .find(|t| t.question.is_some())
        .or_else(|| {
            agent
                .running
                .first()
                .and_then(|id| g.tasks.iter().find(|t| t.task == *id))
        })
        .or_else(|| {
            g.tasks
                .iter()
                .find(|t| t.owner == agent.name && t.question.is_some())
        })?;
    if let Some(q) = &task.question {
        return Some(format!(
            "Waiting for your answer: {}",
            labels.approval_line(&q.request)
        ));
    }
    if agent.state == AgentState::Suspended {
        return Some("Paused".into());
    }
    if let Some(on) = task.waiting_on.as_deref().filter(|_| {
        matches!(
            task.state,
            TaskState::Waiting | TaskState::Queued | TaskState::Pending
        )
    }) {
        return Some(format!("Waiting for {on}"));
    }
    Some(match agent.step.as_ref().filter(|s| s.task == task.task) {
        Some(s) => labels.call_label(&s.action, &s.input),
        None => clip(&task.objective, 80),
    })
}

/// Tasks agents run on their own: when, the next run, what.
pub fn schedules_table(g: &Graph) -> Table {
    let mut t = Table::with_header(&["schedule", "agent", "when", "next", "runs", "task"]).right(4);
    for s in &g.schedules {
        t = t.row([
            s.id.clone(),
            s.agent.clone(),
            s.says.clone(),
            match s.next_label() {
                Some(n) => n,
                None if s.paused => "paused".into(),
                None => "-".into(),
            },
            s.runs.to_string(),
            clip(&s.task.objective, 60),
        ]);
    }
    t
}

/// One schedule as both front-ends show it: what, when, the next run, how the last one went.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ScheduleView {
    pub target: Option<String>,
    pub version: u64,
    pub admission_error: Option<String>,
    pub admission_retry_ms: Option<u64>,
    pub id: String,
    pub agent: String,
    pub objective: String,
    pub context: String,
    /// When, in words: `every day at 02:00`.
    pub says: String,
    /// The IANA zone its times are read in.
    pub tz: Option<String>,
    pub next_ms: Option<u64>,
    /// The next run in words, in the schedule's zone: `Thu 02:00`.
    pub next: Option<String>,
    pub paused: bool,
    /// `scheduled`, `running`, `asking`, `retrying`, `succeeded`, `failed`, `paused`.
    pub state: ark_orchestrate::ScheduleState,
    /// The state in words, what matters first: "waits for you", "trying again at 02:10",
    /// "did not finish Thu 02:00", "running now", "paused", else the next run ("next Thu 02:00").
    pub status: String,
    /// How the last run ended, in words: "ran Thu 02:00", "did not finish Thu 02:00".
    pub last: Option<String>,
    /// The chat its results are posted in.
    pub chat: Option<String>,
    pub runs: u32,
    pub missed: u32,
    pub skipped: u32,
    pub last_task: Option<String>,
}

/// One run of a schedule, newest first in [`schedule_runs`].
#[derive(Clone, Debug, serde::Serialize)]
pub struct RunView {
    pub task: String,
    pub state: TaskState,
    /// When it started (or was made), in the schedule's zone: `Thu 02:00`.
    pub at: String,
    pub at_ms: u64,
    pub result: Option<String>,
    pub error: Option<String>,
    /// Its files and other outputs, by artifact id.
    pub outputs: Vec<ark_orchestrate::ArtifactRef>,
    pub attempts: u32,
    /// Why it is not running, while it waits: "trying again at 02:10", "your answer".
    pub waiting_on: Option<String>,
}

/// The schedules of `o`, soonest first, paused last; only those reporting to `chat` when given.
pub fn schedules(o: &ark_orchestrate::Orchestrator, chat: Option<&str>) -> Vec<ScheduleView> {
    o.schedules()
        .into_iter()
        .filter(|s| chat.is_none() || s.task.report_to.as_deref() == chat)
        .map(|s| schedule_view(o, &s))
        .collect()
}

/// A bounded read-only snapshot of persisted schedules and actual runs.
pub fn schedule_calendar(
    o: &ark_orchestrate::Orchestrator,
    from: u64,
    to: u64,
    zone: &str,
) -> anyhow::Result<serde_json::Value> {
    use serde_json::json;
    anyhow::ensure!(
        from < to && to - from <= 62 * 86_400_000 && to < 253_370_764_800_000,
        "Calendar range must be at most 62 days"
    );
    jiff::tz::TimeZone::get(zone).map_err(|_| anyhow::anyhow!("Invalid calendar timezone"))?;
    let instant = |ms: u64| -> anyhow::Result<String> {
        Ok(jiff::Timestamp::from_millisecond(i64::try_from(ms)?)?.to_string())
    };
    let title = |value: &str| {
        let mut units = 0;
        value
            .chars()
            .take_while(|ch| {
                units += ch.len_utf16();
                units <= 240
            })
            .collect::<String>()
    };
    let mut events = Vec::new();
    let snapshot = o.calendar_snapshot(from, to)?;
    let tasks = snapshot.tasks;
    let runs = snapshot.runs;
    let mut occupied: std::collections::HashSet<_> = tasks
        .iter()
        .filter_map(|task| Some((task.schedule.clone()?, task.slot?)))
        .collect();
    for run in &runs {
        if let ark_orchestrate::automation::RunOrigin::Schedule { schedule, slot_ms } = &run.origin
        {
            occupied.insert((schedule.clone(), *slot_ms));
        }
    }
    let records = snapshot.schedules;
    anyhow::ensure!(
        records.len() <= 1000,
        "Too many schedules for one calendar snapshot"
    );
    for record in records {
        let revision =
            ark_sdk::prelude::ArtifactId::of_bytes(&serde_json::to_vec(&record)?).to_string();
        let record_title = title(&record.task.objective);
        let mut add = |id: String,
                       start: u64,
                       end: u64,
                       state: &str,
                       revision: &str,
                       action: &str|
         -> anyhow::Result<()> {
            if start < to && end > from {
                anyhow::ensure!(
                    events.len() < 1000,
                    "Calendar has more than 1000 occurrences; choose a shorter period"
                );
                events.push(json!({"id": id, "title": record_title, "start": instant(start)?, "end": instant(end.max(start + 1))?,
                    "all_day": false, "end_known": false, "state": state, "delivery": "agent_run", "source_record": record.id,
                    "source_revision": revision, "next_action": action}));
            }
            Ok(())
        };
        if !record.paused {
            if let Some(mut next) = record.next_ms {
                if next < from && !record.when.once {
                    next = if let Some(minutes) = record.when.every_minutes {
                        let step = u64::from(minutes) * 60_000;
                        anyhow::ensure!(step >= 300_000, "Invalid stored schedule interval");
                        next + (from - next).div_ceil(step) * step
                    } else {
                        record.when.next_after(from.saturating_sub(1)).unwrap_or(to)
                    };
                }
                while next < to {
                    if !occupied.contains(&(record.id.clone(), next)) {
                        add(
                            format!("{}-{next}", record.id),
                            next,
                            next + 60_000,
                            "planned",
                            &revision,
                            "Scheduled start. Duration is not known. A run does not prove publication.",
                        )?;
                    }
                    if record.when.once {
                        break;
                    }
                    let Some(after) = record.when.next_after(next) else {
                        break;
                    };
                    anyhow::ensure!(after > next, "Stored schedule did not advance");
                    next = after;
                }
            }
        }
    }
    for task in tasks {
        let start = task.started_ms.unwrap_or(task.created_ms);
        let end = task
            .finished_ms
            .unwrap_or(start.saturating_add(60_000))
            .max(start.saturating_add(1));
        if start >= to || end <= from {
            continue;
        }
        anyhow::ensure!(
            events.len() < 1000,
            "Calendar has more than 1000 occurrences; choose a shorter period"
        );
        let state = match task.state {
            TaskState::Done => "completed",
            TaskState::Failed => "failed",
            TaskState::Cancelled => "cancelled",
            TaskState::Waiting | TaskState::Suspended => "needs_you",
            TaskState::Running => "running",
            _ => "planned",
        };
        let revision =
            ark_sdk::prelude::ArtifactId::of_bytes(&serde_json::to_vec(&task)?).to_string();
        events.push(json!({"id": format!("run-{}", task.id), "title": title(&task.objective),
            "start": instant(start)?, "end": instant(end)?, "all_day":false, "end_known": task.finished_ms.is_some(), "state":state, "delivery":"agent_run",
            "source_record":task.schedule, "source_revision":revision, "next_action":"Recorded run. An open run has no recorded finish time."}));
    }
    for run in runs {
        use ark_orchestrate::automation::{RunOrigin, RunPhase};
        let start = run.created_ms;
        let end = if run.phase.is_final() {
            run.updated_ms.max(start + 1)
        } else {
            start.saturating_add(60_000)
        };
        if start >= to || run.phase.is_final() && end <= from {
            continue;
        }
        anyhow::ensure!(
            events.len() < 1000,
            "Calendar has more than 1000 occurrences; choose a shorter period"
        );
        let state = match run.phase {
            RunPhase::Pending => "planned",
            RunPhase::Running => "running",
            RunPhase::WaitingApproval | RunPhase::Interrupted => "needs_you",
            RunPhase::Completed => "completed",
            RunPhase::Failed => "failed",
            RunPhase::Cancelled => "cancelled",
        };
        let source = match &run.origin {
            RunOrigin::Schedule { schedule, .. } | RunOrigin::ScheduleManual { schedule, .. } => {
                schedule
            }
            _ => &run.automation,
        };
        events.push(json!({"id":format!("automation-{}",run.id),"title":title(&run.revision.draft.name),"start":instant(start)?,"end":instant(end.max(start+1))?,"all_day":false,"end_known":run.phase.is_final(),"state":state,"delivery":"agent_run","source_record":source,"source_revision":run.revision.digest,"next_action":"Recorded execution. Open Work for its conversation."}));
    }
    events.sort_by(|a, b| {
        a["start"]
            .as_str()
            .cmp(&b["start"].as_str())
            .then(a["id"].as_str().cmp(&b["id"].as_str()))
    });
    let snapshot = json!({"type": "ark.calendar", "version": 1, "title": "Scheduled work", "timezone": zone,
        "generated_at": instant(u64::try_from(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis())?)?, "events": events});
    anyhow::ensure!(
        serde_json::to_vec(&snapshot)?.len() <= 1_048_576,
        "Calendar exceeds the 1 MiB limit"
    );
    Ok(snapshot)
}

pub fn schedule_view(
    o: &ark_orchestrate::Orchestrator,
    s: &ark_orchestrate::ScheduleRecord,
) -> ScheduleView {
    use ark_orchestrate::ScheduleState as S;
    let state = o.schedule_state(s);
    let last = s.last_task.as_deref().and_then(|t| o.task(t));
    let when = |ms: u64| s.when.label(ms);
    let ended = last.as_ref().filter(|t| t.state.is_final()).and_then(|t| {
        let at = when(t.finished_ms.unwrap_or(t.created_ms));
        match t.state {
            TaskState::Done => Some(format!("ran {at}")),
            TaskState::Failed => Some(format!("did not finish {at}")),
            _ => None,
        }
    });
    let status = match (state, &last) {
        (S::Asking, _) => "waits for you".to_string(),
        (S::Retrying, Some(t)) => match t.retry_at_ms {
            Some(at) => format!("trying again at {}", when(at)),
            None => "trying again".into(),
        },
        (S::Running, _) => "running now".into(),
        (S::Failed, _) => ended.clone().unwrap_or_default(),
        (S::Paused, _) => "paused".into(),
        _ => match s.next_label() {
            Some(n) => format!("next {n}"),
            None => ended.clone().unwrap_or_default(),
        },
    };
    ScheduleView {
        target: s.target.clone(),
        version: s.version,
        admission_error: s.admission_error.clone(),
        admission_retry_ms: s.admission_retry_ms,
        id: s.id.clone(),
        agent: s.agent.clone(),
        objective: s.task.objective.clone(),
        context: s.task.context.clone(),
        says: s.says.clone(),
        tz: s.when.tz.clone(),
        next_ms: s.next_ms,
        next: s.next_label(),
        paused: s.paused,
        state,
        status,
        last: ended,
        chat: s.task.report_to.clone(),
        runs: s.runs,
        missed: s.missed,
        skipped: s.skipped,
        last_task: s.last_task.clone(),
    }
}

/// A schedule's runs, newest first, at most `limit`.
pub fn schedule_runs(o: &ark_orchestrate::Orchestrator, id: &str, limit: usize) -> Vec<RunView> {
    let Some(s) = o.schedule_record(id) else {
        return Vec::new();
    };
    o.schedule_runs(id, limit)
        .into_iter()
        .map(|t| {
            let at_ms = t.started_ms.unwrap_or(t.created_ms);
            RunView {
                at: s.when.label(at_ms),
                at_ms,
                task: t.id,
                state: t.state,
                result: t.result,
                error: t.error.map(|e| crate::labels::person_error(&e)),
                outputs: t.outputs,
                attempts: t.attempts,
                // In the person's words: a busy agent is a run waiting its turn.
                waiting_on: t.waiting_on.filter(|_| !t.state.is_final()).map(|w| {
                    if w.starts_with("agent ") || w.starts_with("capacity") {
                        "its turn".to_string()
                    } else {
                        w
                    }
                }),
            }
        })
        .collect()
}

/// What runs on its own in this project (`/schedules`).
pub fn schedules_report(o: &ark_orchestrate::Orchestrator) -> Report {
    let r = Report::titled("Scheduled");
    let all = schedules(o, None);
    if all.is_empty() {
        return r.note("Nothing is scheduled. Ask in words: \"every day at 2 AM, recount the stock and send me a summary\".");
    }
    let mut t = Table::with_header(&["schedule", "when", "now", "runs", "task"]).right(3);
    for v in &all {
        t = t.row([
            v.id.clone(),
            v.says.clone(),
            v.status.clone(),
            v.runs.to_string(),
            clip(&v.objective, 60),
        ]);
    }
    r.table(t)
        .note("/schedules run|pause|resume|stop <schedule>, or ask in words.")
}

/// The whole graph: tasks as a tree (who delegated what to whom), agents with their model,
/// session and context use, and each account's capacity.
pub fn graph_report(g: &Graph, labels: &Labels) -> Report {
    graph_report_with_workers(g, labels, None)
}

/// [`graph_report`] with the app's names for agent programs. Technical records keep the saved
/// spec; the ordinary command view says the same name as the window.
pub fn graph_report_with_workers(
    g: &Graph,
    labels: &Labels,
    workers: Option<&dyn ark_sdk::prelude::ark_runtime::worker::Workers>,
) -> Report {
    let mut r = Report::titled("Agents and tasks");
    if g.tasks.is_empty() && g.agents.is_empty() {
        return r.note("No agents yet.");
    }
    if !g.tasks.is_empty() {
        let mut t = Table::with_header(&["task", "agent", "state", "tokens", "time", "objective"])
            .right(3)
            .right(4);
        for root in g.roots() {
            push_tree(g, root, 0, &mut t);
        }
        r = r.heading("Tasks").table(t);
    }
    let mut a = Table::with_header(&[
        "agent", "state", "doing", "model", "session", "context", "tokens", "done",
    ])
    .right(6)
    .right(7);
    for n in &g.agents {
        let session = match &n.session {
            None => "-".to_string(),
            Some(s) => {
                let st = match s.state {
                    SessionState::Busy => "busy",
                    SessionState::Idle => "open",
                    SessionState::Closed => "closed",
                };
                let mut v = format!("{st} · {} turns", s.turns);
                if s.rotations > 0 {
                    v.push_str(&format!(" · {} rotated", s.rotations));
                }
                v
            }
        };
        let context = match (&n.session, &n.context) {
            (Some(s), _) if s.context_tokens > 0 => format!(
                "{} / {}",
                tokens(s.context_tokens as u64),
                tokens(s.max_context_tokens as u64)
            ),
            (_, Some(c)) => format!("{} / {}", tokens(c.tokens as u64), tokens(c.budget as u64)),
            _ => "-".into(),
        };
        let tone = match n.state {
            AgentState::Running => Tone::Accent,
            AgentState::Waiting | AgentState::Suspended => Tone::Warning,
            AgentState::Terminated => Tone::Muted,
            AgentState::Idle => Tone::Normal,
        };
        a.push(Row {
            cells: vec![
                Cell::new(n.name.clone(), Tone::Normal),
                Cell::new(agent_word(n.state), tone),
                Cell::new(
                    doing(g, n, labels)
                        .map(|d| clip(&d, 60))
                        .unwrap_or_default(),
                    Tone::Normal,
                ),
                // The profile it starts from, when the app's and not the base one.
                Cell::new(
                    match &n.profile {
                        Some(p) => format!("{} · {p}", worker_name(&n.model, workers)),
                        None => worker_name(&n.model, workers),
                    },
                    Tone::Muted,
                ),
                Cell::new(session, Tone::Muted),
                Cell::new(context, Tone::Muted),
                Cell::new(tokens(n.usage.tokens()), Tone::Normal),
                Cell::new(format!("{}", n.tasks_done), Tone::Normal),
            ],
            tone: Tone::Normal,
            command: None,
        });
    }
    r = r.heading("Agents").table(a);
    if !g.schedules.is_empty() {
        r = r.heading("Scheduled").table(schedules_table(g));
    }
    if !g.accounts.is_empty() {
        let mut c = Table::with_header(&["account", "in use", "waiting", "started"])
            .right(1)
            .right(2)
            .right(3);
        for acct in &g.accounts {
            c = c.row([
                acct.account.clone(),
                format!("{} / {}", acct.in_flight, acct.max_concurrent),
                acct.waiting.to_string(),
                acct.started.to_string(),
            ]);
        }
        r = r.heading("Capacity").table(c);
    }
    for t in g.tasks.iter().filter(|t| t.question.is_some()) {
        r = r.note(format!(
            "{} waits for your answer. `/task answer {} once|session|always|deny`",
            t.task, t.task
        ));
    }
    r.note(format!(
        "{} · {} tokens. `/agents <task>` shows one task's trace.",
        count(g.tasks.len() as u64, "task"),
        tokens(g.usage.tokens())
    ))
    .data("orchestration", g)
}

fn worker_name(
    model: &str,
    workers: Option<&dyn ark_sdk::prelude::ark_runtime::worker::Workers>,
) -> String {
    if !model.starts_with(ark_sdk::prelude::ark_runtime::worker::SPEC) {
        return model.to_string();
    }
    workers
        .and_then(|workers| {
            workers.offers().into_iter().find_map(|offer| {
                if offer.spec == model {
                    return Some(offer.title);
                }
                offer
                    .models
                    .into_iter()
                    .find(|choice| choice.spec == model)
                    .map(|choice| choice.title)
            })
        })
        .unwrap_or_else(|| "Unavailable".into())
}

fn push_tree(g: &Graph, t: &TaskNode, depth: usize, table: &mut Table) {
    let indent = "  ".repeat(depth);
    let state = match &t.waiting_on {
        Some(w) if !t.state.is_final() => format!("{} ({})", t.state.as_str(), clip(w, 40)),
        _ => t.state.as_str().to_string(),
    };
    table.push(Row {
        cells: vec![
            Cell::new(format!("{indent}{}", t.task), Tone::Normal),
            Cell::new(t.owner.clone(), Tone::Accent),
            Cell::new(state, task_tone(t.state)),
            Cell::new(tokens(t.usage.tokens()), Tone::Normal),
            Cell::new(millis(t.run_ms), Tone::Muted),
            Cell::new(clip(&t.objective, 60), Tone::Muted),
        ],
        tone: Tone::Normal,
        command: Some(format!("/agents {}", t.task)),
    });
    for c in g.children_of(&t.task) {
        push_tree(g, c, depth + 1, table);
    }
}

/// One task: what it was given, what it did, what it returned, and its trace.
pub fn task_report(g: &Graph, task: &str, events: &[OrchEvent]) -> Report {
    task_report_with_workers(g, task, events, None)
}

pub fn task_report_with_workers(
    g: &Graph,
    task: &str,
    events: &[OrchEvent],
    workers: Option<&dyn ark_sdk::prelude::ark_runtime::worker::Workers>,
) -> Report {
    let Some(t) = g.task(task) else {
        return Report::new().error(format!("no task `{task}`"));
    };
    let by = match &t.origin {
        Origin::External => "the application".to_string(),
        Origin::Agent { agent, task } => format!("{agent} (task {task})"),
    };
    let mut r = Report::titled(format!("Task {} · {}", t.task, t.owner))
        .field_toned("state", t.state.as_str(), task_tone(t.state))
        .field("asked by", by)
        .field("objective", clip(&t.objective, 300))
        .field("model", worker_name(&t.model, workers))
        .field(
            "brief",
            format!("{} tokens (all the agent was given)", t.brief_tokens),
        )
        .field(
            "usage",
            format!(
                "{} tokens · {} · {} · {} steps",
                tokens(t.usage.tokens()),
                count(t.usage.model_calls as u64, "model call"),
                count(t.usage.tool_calls as u64, "tool call"),
                t.usage.steps
            ),
        )
        .field(
            "time",
            format!(
                "{} running · {} total",
                millis(t.run_ms),
                millis(t.elapsed_ms)
            ),
        );
    if t.attempts > 1 {
        r = r.field("attempts", format!("{} of {}", t.attempts, t.max_attempts));
    }
    if !t.rules.is_empty() || t.rules_held_back > 0 {
        let ids: Vec<String> = t.rules.iter().map(|x| x.to_string()).collect();
        r = r.field(
            "rules",
            format!("{} ({} held back)", ids.join(", "), t.rules_held_back),
        );
    }
    if !t.inputs.is_empty() {
        let names: Vec<String> = t.inputs.iter().map(|a| a.name.clone()).collect();
        r = r.field("inputs", names.join(", "));
    }
    if !t.outputs.is_empty() {
        let names: Vec<String> = t.outputs.iter().map(|a| a.name.clone()).collect();
        r = r.field("produced", names.join(", "));
    }
    if let Some(e) = &t.error {
        r = r.error(e.clone());
    }
    if let Some(res) = &t.result {
        r = r.heading("Result").text(res.clone());
    }
    let mut timeline = Table::plain(3);
    let start = events.first().map(|e| e.at_ms).unwrap_or(0);
    for e in events {
        let Some((text, tone)) = event_line(&e.kind) else {
            continue;
        };
        timeline.push(Row {
            cells: vec![
                Cell::new(
                    format!("+{}", millis(e.at_ms.saturating_sub(start))),
                    Tone::Muted,
                ),
                Cell::new(e.kind.agent().unwrap_or("").to_string(), Tone::Accent),
                Cell::new(text, tone),
            ],
            tone: Tone::Normal,
            command: match &e.kind {
                EventKind::Delegated { task, .. } => Some(format!("/agents {task}")),
                _ => None,
            },
        });
    }
    if !timeline.rows.is_empty() {
        r = r.heading("Trace").table(timeline);
    }
    r.data(
        "orchestration_task",
        serde_json::json!({ "task": t, "events": events }),
    )
}

/// One line for an event in a task's timeline. `None` for events a timeline leaves out.
pub fn event_line(k: &EventKind) -> Option<(String, Tone)> {
    Some(match k {
        EventKind::TaskCreated { objective, .. } => {
            (format!("created: {}", clip(objective, 80)), Tone::Normal)
        }
        EventKind::TaskQueued { reason, .. } => (format!("waiting: {reason}"), Tone::Warning),
        EventKind::TaskStarted {
            attempt,
            model,
            brief_tokens,
            ..
        } => (
            format!("started (attempt {attempt}) on {model} with a {brief_tokens}-token brief"),
            Tone::Normal,
        ),
        EventKind::Delegated { to_agent, task, .. } => {
            (format!("delegated {task} to {to_agent}"), Tone::Accent)
        }
        EventKind::TaskWaiting { on, .. } => {
            (format!("waiting for {}", on.join(", ")), Tone::Warning)
        }
        EventKind::TaskResumed { .. } => ("continues".into(), Tone::Normal),
        EventKind::Asked { preview, .. } => (
            format!("waiting for your answer: {}", clip(preview, 80)),
            Tone::Warning,
        ),
        EventKind::Answered { decision, .. } => (
            if decision.allows() {
                "you allowed it".into()
            } else {
                "you declined it".into()
            },
            Tone::Normal,
        ),
        EventKind::Message { from, to, text, .. } => (
            format!("message {from} → {to}: {}", clip(text, 80)),
            Tone::Normal,
        ),
        EventKind::RulesApplied {
            applied, held_back, ..
        } => (
            format!(
                "rules: {} ({held_back} held back)",
                applied
                    .iter()
                    .map(|r| r.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Tone::Muted,
        ),
        // The orchestrator reports the task's rules itself (`RulesApplied`).
        EventKind::Agent {
            event: ark_sdk::prelude::HarnessEvent::Rules { .. },
            ..
        } => return None,
        EventKind::Agent { event, .. } => {
            let (_, text, detail, tone) = crate::trace::entry(event)?;
            match detail {
                Some(d) if !d.is_empty() => (format!("{text} · {}", clip(&d, 100)), tone),
                _ => (text, tone),
            }
        }
        EventKind::ArtifactProduced { artifact, .. } => (
            format!("produced {} ({})", artifact.name, artifact.kind),
            Tone::Success,
        ),
        EventKind::TaskRetry { attempt, error, .. } => (
            format!("retry {attempt}: {}", clip(error, 100)),
            Tone::Warning,
        ),
        EventKind::BudgetExceeded { limit, .. } => {
            (format!("budget exceeded: {limit}"), Tone::Error)
        }
        EventKind::TaskFinished { state, error, .. } => (
            match error {
                Some(e) => format!("{}: {}", state.as_str(), clip(e, 100)),
                None => state.as_str().to_string(),
            },
            task_tone(*state),
        ),
        EventKind::Scheduled {
            agent,
            objective,
            says,
            ..
        } => (
            format!("{agent} runs {says}: {}", clip(objective, 80)),
            Tone::Accent,
        ),
        EventKind::Posted { post, .. } => (
            "posted the result in the chat that asked for it".to_string(),
            if post.ok { Tone::Muted } else { Tone::Warning },
        ),
        EventKind::Unscheduled { agent, says, .. } => {
            (format!("{agent} no longer runs {says}"), Tone::Muted)
        }
        EventKind::ScheduleChanged {
            agent,
            says,
            paused,
            ..
        } => (
            if *paused {
                format!("{agent}'s schedule ({says}) paused")
            } else {
                format!("{agent} runs {says}")
            },
            Tone::Muted,
        ),
        EventKind::Work { item, by } => (
            format!(
                "{} {}: {}",
                who(by),
                item.log
                    .last()
                    .map(|s| s.text.as_str())
                    .unwrap_or("changed"),
                clip(&item.title, 60)
            ),
            Tone::Normal,
        ),
        EventKind::WorkRemoved { title, .. } => (
            format!("removed from the board: {}", clip(title, 60)),
            Tone::Muted,
        ),
        EventKind::Board { board, by } => (
            format!("{} keeps the board \"{}\"", who(by), clip(&board.goal, 60)),
            Tone::Normal,
        ),
        EventKind::BoardRemoved { goal, .. } => (
            format!("removed the board \"{}\"", clip(goal, 60)),
            Tone::Muted,
        ),
        EventKind::Automation { .. }
        | EventKind::AgentSpawned { .. }
        | EventKind::AgentState { .. }
        | EventKind::Session { .. } => return None,
    })
}

/// A board actor in words: the person is "you".
pub fn who(by: &str) -> String {
    match by {
        PERSON => "you".into(),
        other => other.into(),
    }
}

/// The project's boards (`/board`), the latest first: each one's open work by column and the
/// latest done.
pub fn board_report(boards: &[Board], items: &[WorkItem]) -> Report {
    let r = Report::titled("Boards");
    if boards.is_empty() {
        return r.note(
            "There are no boards yet. Add an item with /board add <title>, or ask in words.",
        );
    }
    let mut r = r;
    for b in boards {
        r = r.heading(format!("{} · {}", b.goal, b.id));
        let on: Vec<&WorkItem> = items.iter().filter(|w| w.board == b.id).collect();
        if on.is_empty() {
            r = r.note("Nothing on it yet.");
        }
        r = board_columns(r, &on);
    }
    r.note("/board add <title> · done <item> · move <item> todo|doing|done · give <item> <agent> · remove <item>")
}

fn board_columns(mut r: Report, items: &[&WorkItem]) -> Report {
    for state in WorkState::ALL {
        let col: Vec<&WorkItem> = items.iter().copied().filter(|w| w.state == state).collect();
        if col.is_empty() {
            continue;
        }
        let shown = if state == WorkState::Done {
            5
        } else {
            col.len()
        };
        let mut t = Table::with_header(&["item", "owner", "title"]);
        for w in col.iter().take(shown) {
            let mut title = clip(&w.title, 70);
            if let Some(b) = &w.blocked {
                title.push_str(&format!("  (blocked: {})", clip(b, 50)));
            }
            t = t.row([
                Cell::from(w.id.clone()),
                Cell::from(w.owner.as_deref().map(who).unwrap_or_else(|| "-".into())),
                if w.blocked.is_some() {
                    Cell::new(title, Tone::Warning)
                } else {
                    Cell::from(title)
                },
            ]);
        }
        r = r
            .heading(format!("{} ({})", state.label(), col.len()))
            .table(t);
    }
    r
}

// ---- control ---------------------------------------------------------------------------------

/// One change a person asks of the agents. Both front-ends build these (the window from its
/// protocol methods, the terminal from `/agent` and `/task`) and run them through
/// [`control`], so a change means the same wherever it was asked for.
#[derive(Clone, Debug)]
pub enum Control {
    /// Add an agent, or change one (an ended agent comes back).
    Spawn(Box<ark_orchestrate::AgentSpec>),
    /// Hand an agent a task.
    Submit {
        agent: String,
        task: ark_orchestrate::TaskSpec,
    },
    /// Stop a task and what it delegated.
    Cancel {
        task: String,
    },
    /// Run a failed or cancelled task again.
    Retry {
        task: String,
    },
    /// Pause an agent: its running task stops at the next step and waits.
    Suspend {
        agent: String,
    },
    Resume {
        agent: String,
    },
    /// End an agent: its open tasks are cancelled and its model session closes.
    Terminate {
        agent: String,
    },
    /// A note from the person: into the agent's running task, else its next one.
    /// `thread`: one of the agent's conversations; `None`: its default one.
    Message {
        agent: String,
        text: String,
        thread: Option<String>,
    },
    MessageWithContext {
        agent: String,
        text: String,
        context: String,
        thread: Option<String>,
    },
    /// Stop a schedule (a run already started goes on).
    Unschedule {
        schedule: String,
    },
    /// Keep a schedule, run nothing until resumed.
    PauseSchedule {
        schedule: String,
    },
    ResumeSchedule {
        schedule: String,
    },
    /// Start a schedule's run now, besides its times.
    RunSchedule {
        schedule: String,
    },
    /// Add an item to the board.
    AddWork {
        title: String,
        change: WorkChange,
    },
    /// Change a board item (its words, column, place, owner, what blocks it).
    ChangeWork {
        item: String,
        change: WorkChange,
    },
    RemoveWork {
        item: String,
    },
    /// Hand a board item to an agent: a task with the item's title, linked to it.
    GiveWork {
        item: String,
        agent: String,
    },
    /// Give a board another goal.
    ChangeBoard {
        board: String,
        goal: String,
    },
    /// Remove a board and its items.
    RemoveBoard {
        board: String,
    },
    /// Answer the question a task waits on (ADR 0036).
    Answer {
        task: String,
        decision: ApprovalDecision,
    },
}

/// What a [`Control`] did.
#[derive(Clone, Debug)]
pub enum ControlOutcome {
    Agent(Box<ark_orchestrate::AgentRecord>),
    Submitted {
        task: String,
    },
    AutomationSubmitted {
        run: String,
        work: String,
        conversation: String,
    },
    /// `delivered`: it reached a running task (else it waits for the next one).
    Messaged {
        agent: String,
        delivered: bool,
    },
    Work(Box<WorkItem>),
    Board(Box<Board>),
    Done(String),
}

impl ControlOutcome {
    /// The one line a front-end shows for it.
    pub fn report(&self) -> Report {
        match self {
            ControlOutcome::Agent(a) => Report::new().success(format!("{} is ready", a.name)),
            ControlOutcome::Submitted { task } => Report::new()
                .success(format!("started task {task}"))
                .note(format!("Follow it with /agents {task}")),
            ControlOutcome::AutomationSubmitted { run, .. } => {
                Report::new().success(format!("started execution {run}"))
            }
            ControlOutcome::Messaged { agent, delivered } => Report::new().success(if *delivered {
                format!("sent into {agent}'s running task")
            } else {
                format!("kept for {agent}'s next task")
            }),
            ControlOutcome::Work(w) => Report::new().success(w.line()),
            ControlOutcome::Board(b) => Report::new().success(format!("{} · {}", b.goal, b.id)),
            ControlOutcome::Done(what) => Report::new().success(what.clone()),
        }
    }
}

/// Run one [`Control`] on `o`.
pub async fn control(
    o: &ark_orchestrate::Orchestrator,
    c: Control,
) -> anyhow::Result<ControlOutcome> {
    Ok(match c {
        Control::Spawn(spec) => ControlOutcome::Agent(Box::new(o.spawn(*spec).await?)),
        Control::Submit { agent, mut task } => {
            task.person = true;
            if task.objective.trim().is_empty() {
                anyhow::bail!("a task needs an objective");
            }
            ControlOutcome::Submitted {
                task: o.submit(&agent, task)?,
            }
        }
        Control::Cancel { task } => {
            o.cancel_and_wait(&task).await?;
            ControlOutcome::Done(format!("cancelled {task}"))
        }
        Control::Retry { task } => {
            o.retry(&task)?;
            ControlOutcome::Done(format!("running {task} again"))
        }
        Control::Suspend { agent } => {
            o.suspend(&agent)?;
            ControlOutcome::Done(format!("{agent} paused"))
        }
        Control::Resume { agent } => {
            o.resume(&agent)?;
            ControlOutcome::Done(format!("{agent} resumed"))
        }
        Control::Terminate { agent } => {
            o.terminate(&agent).await?;
            ControlOutcome::Done(format!("{agent} ended"))
        }
        Control::Message {
            agent,
            text,
            thread,
        } => {
            if text.trim().is_empty() {
                anyhow::bail!("a message needs text");
            }
            let delivered = o.message_thread("user", &agent, thread.as_deref(), &text, "")?;
            ControlOutcome::Messaged { agent, delivered }
        }
        Control::MessageWithContext {
            agent,
            text,
            context,
            thread,
        } => {
            let delivered = o.message_thread("user", &agent, thread.as_deref(), &text, &context)?;
            ControlOutcome::Messaged { agent, delivered }
        }
        Control::Unschedule { schedule } => {
            let s = o.stop_schedule(&schedule).await?;
            ControlOutcome::Done(format!(
                "{} no longer runs \"{}\"",
                s.agent,
                clip(&s.task.objective, 60)
            ))
        }
        Control::PauseSchedule { schedule } => {
            let s = o.pause_schedule(&schedule)?;
            ControlOutcome::Done(format!("paused \"{}\"", clip(&s.task.objective, 60)))
        }
        Control::ResumeSchedule { schedule } => {
            let s = o.resume_schedule(&schedule)?;
            ControlOutcome::Done(format!(
                "\"{}\" runs {}, next {}",
                clip(&s.task.objective, 60),
                s.says,
                s.next_label().unwrap_or_default()
            ))
        }
        Control::RunSchedule { schedule } => {
            let execution = o.run_schedule_now(&schedule).await?;
            if o.task(&execution).is_some() {
                ControlOutcome::Submitted { task: execution }
            } else {
                let run = o.automation_run(&execution)?;
                ControlOutcome::AutomationSubmitted {
                    run: run.id,
                    work: run.work,
                    conversation: run.conversation,
                }
            }
        }
        Control::AddWork { title, change } => {
            ControlOutcome::Work(Box::new(o.add_work(PERSON, &title, change)?))
        }
        Control::ChangeWork { item, change } => {
            ControlOutcome::Work(Box::new(o.change_work(PERSON, &item, change)?))
        }
        Control::RemoveWork { item } => {
            let w = o.remove_work(&item)?;
            ControlOutcome::Done(format!("removed \"{}\"", clip(&w.title, 60)))
        }
        Control::Answer { task, decision } => {
            o.answer(&task, decision)?;
            ControlOutcome::Done(format!(
                "{task}: {}",
                if decision.allows() {
                    "allowed"
                } else {
                    "declined"
                }
            ))
        }
        Control::ChangeBoard { board, goal } => {
            ControlOutcome::Board(Box::new(o.change_board(PERSON, &board, &goal)?))
        }
        Control::RemoveBoard { board } => {
            let b = o.remove_board(&board)?;
            ControlOutcome::Done(format!("removed the board \"{}\"", clip(&b.goal, 60)))
        }
        Control::GiveWork { item, agent } => {
            let w = o
                .work_item(&item)
                .ok_or_else(|| anyhow::anyhow!("no board item `{item}`"))?;
            let task = ark_orchestrate::TaskSpec::new(w.title.clone()).item(w.id.clone());
            ControlOutcome::Submitted {
                task: o.submit(&agent, task)?,
            }
        }
    })
}

/// `/board …` as a [`Control`]: `add <title>`, `done <item>`, `move <item> <column>`,
/// `give <item> <agent>`, `remove <item>`.
pub fn parse_board(args: &str) -> Result<Control, String> {
    let usage = || {
        "usage: /board add <title> | done <item> | move <item> todo|doing|done | give <item> <agent> | remove <item>".to_string()
    };
    let args = args.trim();
    let (verb, rest) = args.split_once(char::is_whitespace).unwrap_or((args, ""));
    let rest = rest.trim();
    let mut words = rest.split_whitespace();
    let item = words.next().unwrap_or("").to_string();
    Ok(match verb {
        "add" if !rest.is_empty() => Control::AddWork {
            title: rest.to_string(),
            change: WorkChange::default(),
        },
        "done" if !item.is_empty() => Control::ChangeWork {
            item,
            change: WorkChange::default().state(WorkState::Done),
        },
        "move" => {
            let state = words.collect::<Vec<_>>().join(" ");
            match (item.is_empty(), WorkState::parse(&state)) {
                (false, Some(s)) => Control::ChangeWork {
                    item,
                    change: WorkChange::default().state(s),
                },
                _ => return Err(usage()),
            }
        }
        "give" => match words.next() {
            Some(agent) if !item.is_empty() => Control::GiveWork {
                item,
                agent: agent.to_string(),
            },
            _ => return Err(usage()),
        },
        "remove" if !item.is_empty() => Control::RemoveWork { item },
        _ => return Err(usage()),
    })
}

/// `/agent …` as a [`Control`]: `new <name> [provider:model]`, `pause|resume|end <name>`,
/// `message <name> <text>`.
pub fn parse_agent(args: &str) -> Result<Control, String> {
    let mut it = args.split_whitespace();
    let verb = it.next().unwrap_or("");
    let name = it.next().map(str::to_string);
    let need = |n: Option<String>| n.ok_or_else(|| format!("/agent {verb} needs an agent name"));
    Ok(match verb {
        "new" => {
            let mut spec = ark_orchestrate::AgentSpec::new(need(name)?);
            if let Some(model) = it.next() {
                spec = spec.model(model);
            }
            Control::Spawn(Box::new(spec))
        }
        "pause" => Control::Suspend { agent: need(name)? },
        "resume" => Control::Resume { agent: need(name)? },
        "end" => Control::Terminate { agent: need(name)? },
        "message" => Control::Message {
            agent: need(name)?,
            text: it.collect::<Vec<_>>().join(" "),
            thread: None,
        },
        _ => {
            return Err(
                "usage: /agent new <name> [provider:model] | pause|resume|end <name> | message <name> <text>"
                    .into(),
            )
        }
    })
}

/// `/task …` as a [`Control`]: `cancel <id>`, `retry <id>`, or `<agent> <objective>`.
pub fn parse_task(args: &str) -> Result<Control, String> {
    let args = args.trim();
    let (first, rest) = args.split_once(char::is_whitespace).unwrap_or((args, ""));
    let rest = rest.trim();
    Ok(match first {
        "" => return Err(
            "usage: /task <agent> <objective> | cancel <task> | retry <task> | answer <task> once|session|always|deny".into(),
        ),
        "answer" => {
            let mut w = rest.split_whitespace();
            let (Some(task), Some(word), None) = (w.next(), w.next(), w.next()) else {
                return Err("usage: /task answer <task> once|session|always|deny".into());
            };
            let decision = decision(word)?;
            Control::Answer {
                task: task.into(),
                decision,
            }
        }
        "cancel" if !rest.is_empty() => Control::Cancel { task: rest.into() },
        "retry" if !rest.is_empty() => Control::Retry { task: rest.into() },
        agent => Control::Submit {
            agent: agent.into(),
            task: ark_orchestrate::TaskSpec::new(rest),
        },
    })
}

/// An answer typed in a terminal: once, session, always or deny.
pub(crate) fn decision(word: &str) -> Result<ApprovalDecision, String> {
    Ok(match word {
        "once" | "yes" | "allow" => ApprovalDecision::AllowOnce,
        "session" => ApprovalDecision::AllowSession,
        "always" => ApprovalDecision::AllowAlways,
        "deny" | "no" => ApprovalDecision::Deny,
        _ => return Err(format!("`{word}`: answer once, session, always or deny")),
    })
}
