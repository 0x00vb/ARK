//! Orchestration views: the agent and task graph, and one task's trace, as [`Report`]s. The
//! terminal prints them; the window renders the same report or uses its typed data
//! (`view: "orchestration"` carries the whole [`Graph`], `view: "orchestration_task"` the
//! task and its events). Both come from the orchestrator's own records and event log.

use ark_orchestrate::{AgentState, EventKind, Graph, OrchEvent, Origin, TaskNode, TaskState};
use ark_sdk::prelude::ark_model::SessionState;

use crate::fmt::{clip, count, millis, tokens};
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

/// Tasks agents run on their own: when, the next run, what.
pub fn schedules_table(g: &Graph) -> Table {
    let mut t = Table::with_header(&["schedule", "agent", "when", "next", "runs", "task"]).right(4);
    for s in &g.schedules {
        t = t.row([
            s.id.clone(),
            s.agent.clone(),
            s.says.clone(),
            s.next_ms
                .map(ark_orchestrate::schedule::local_label)
                .unwrap_or_else(|| "-".into()),
            s.runs.to_string(),
            clip(&s.task.objective, 60),
        ]);
    }
    t
}

/// What runs on its own in this project (`/schedules`).
pub fn schedules_report(g: &Graph) -> Report {
    let r = Report::titled("Scheduled");
    if g.schedules.is_empty() {
        return r.note("Nothing is scheduled. Ask in words: \"every weekday at 9, have the researcher summarize the news\".");
    }
    r.table(schedules_table(g))
        .note("Stop one with /schedules stop <schedule>, or ask in words.")
}

/// The whole graph: tasks as a tree (who delegated what to whom), agents with their model,
/// session and context use, and each account's capacity.
pub fn graph_report(g: &Graph) -> Report {
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
        "agent", "state", "model", "session", "context", "tokens", "done",
    ])
    .right(5)
    .right(6);
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
                // The profile it starts from, when the app's and not the base one.
                Cell::new(
                    match &n.profile {
                        Some(p) => format!("{} · {p}", n.model),
                        None => n.model.clone(),
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
    r.note(format!(
        "{} · {} tokens. `/agents <task>` shows one task's trace.",
        count(g.tasks.len() as u64, "task"),
        tokens(g.usage.tokens())
    ))
    .data("orchestration", g)
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
        .field("model", t.model.clone())
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
        EventKind::Unscheduled { agent, says, .. } => {
            (format!("{agent} no longer runs {says}"), Tone::Muted)
        }
        EventKind::AgentSpawned { .. }
        | EventKind::AgentState { .. }
        | EventKind::Session { .. } => return None,
    })
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
    Message {
        agent: String,
        text: String,
    },
    /// Stop a schedule (a run already started goes on).
    Unschedule {
        schedule: String,
    },
}

/// What a [`Control`] did.
#[derive(Clone, Debug)]
pub enum ControlOutcome {
    Agent(Box<ark_orchestrate::AgentRecord>),
    Submitted {
        task: String,
    },
    /// `delivered`: it reached a running task (else it waits for the next one).
    Messaged {
        agent: String,
        delivered: bool,
    },
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
            ControlOutcome::Messaged { agent, delivered } => Report::new().success(if *delivered {
                format!("sent into {agent}'s running task")
            } else {
                format!("kept for {agent}'s next task")
            }),
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
        Control::Submit { agent, task } => {
            if task.objective.trim().is_empty() {
                anyhow::bail!("a task needs an objective");
            }
            ControlOutcome::Submitted {
                task: o.submit(&agent, task)?,
            }
        }
        Control::Cancel { task } => {
            o.cancel(&task)?;
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
        Control::Message { agent, text } => {
            if text.trim().is_empty() {
                anyhow::bail!("a message needs text");
            }
            let delivered = o.message("user", &agent, &text)?;
            ControlOutcome::Messaged { agent, delivered }
        }
        Control::Unschedule { schedule } => {
            let s = o.unschedule(&schedule)?;
            ControlOutcome::Done(format!(
                "{} no longer runs \"{}\"",
                s.agent,
                clip(&s.task.objective, 60)
            ))
        }
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
        "" => return Err("usage: /task <agent> <objective> | cancel <task> | retry <task>".into()),
        "cancel" if !rest.is_empty() => Control::Cancel { task: rest.into() },
        "retry" if !rest.is_empty() => Control::Retry { task: rest.into() },
        agent => Control::Submit {
            agent: agent.into(),
            task: ark_orchestrate::TaskSpec::new(rest),
        },
    })
}
