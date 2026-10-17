//! The tools an agent uses to work with other agents. Built for one agent and one task, so
//! every call knows who is asking and from which task:
//!
//! - `agents.delegate(to, objective, context, artifacts, wait)`: create a task for a
//!   permitted agent. With `wait` (the default) the call returns the result; several
//!   delegations in one reply run in parallel.
//! - `agents.wait(tasks)`: wait for tasks delegated with `wait: false`.
//! - `agents.message(to, text)`: a short note to a permitted agent, delivered into its running
//!   task or kept for its next one.
//!
//! The person's own conversation gets [`ChatTools`] instead: create an agent, hand any agent a
//! task, wait for tasks, have an agent run a task at set times. So the person asks in words and
//! the chat decides what becomes a task. With [`BoardTools`] it also reads and plans the
//! project's board (`board.list`, `board.add`, `board.update`); a task handed off with `item`
//! moves that item (see [`crate::board`]).
//!
//! The calls are ordinary actions: each becomes an invocation in the delegating agent's
//! store and an evidence card in its context, so a delegation is traced like any tool call
//! and its result is cited like any evidence.

use std::sync::{Arc, Weak};

use ark_core::{ActionDescriptor, RiskTier};
use ark_runtime::Distiller;
use ark_tools::{ActionOutput, Adapter, AdapterError, RunCtx};
use serde_json::{json, Value};

use crate::board::{self, WorkChange, WorkItem, WorkState};
use crate::orchestrator::Inner;
use crate::record::{AgentState, TaskRecord};
use crate::spec::{AgentSpec, TaskSpec};
use crate::Orchestrator;

pub const DELEGATE: &str = "agents.delegate";
pub const WAIT: &str = "agents.wait";
pub const MESSAGE: &str = "agents.message";
pub const CREATE: &str = "agents.create";
pub const SCHEDULE: &str = "agents.schedule";
pub const UNSCHEDULE: &str = "agents.unschedule";
pub const BOARD_LIST: &str = "board.list";
pub const BOARD_ADD: &str = "board.add";
pub const BOARD_UPDATE: &str = "board.update";

/// Who the person's conversation is on the board.
pub const CHAT: &str = "chat";

/// Open items `board.list` returns at most; Done shows the latest few.
pub const BOARD_LISTED: usize = 40;
const DONE_LISTED: usize = 5;

/// Characters of a delegated task's result in the delegator's evidence card. The full result
/// stays in the invocation (`get_evidence`).
pub const RESULT_CHARS: usize = 2400;

pub(crate) struct AgentTools {
    pub orch: Weak<Inner>,
    pub agent: String,
    pub task: String,
    /// Agents it may delegate to: (name, role).
    pub delegates: Vec<(String, String)>,
    /// Agents it may message.
    pub contacts: Vec<String>,
}

#[async_trait::async_trait]
impl Adapter for AgentTools {
    fn name(&self) -> &str {
        "agents"
    }

    fn actions(&self) -> Vec<ActionDescriptor> {
        let mut v = Vec::new();
        if !self.delegates.is_empty() {
            let names: Vec<&str> = self.delegates.iter().map(|(n, _)| n.as_str()).collect();
            v.push(
                ActionDescriptor::new(
                    DELEGATE,
                    "hand a self-contained task to another agent and get its result; independent tasks can be delegated in one reply and run in parallel",
                    RiskTier::Observe,
                )
                .volatile()
                .with_input_schema(json!({
                    "type": "object",
                    "required": ["to", "objective"],
                    "properties": {
                        "to": { "type": "string", "enum": names },
                        "objective": { "type": "string", "description": "what the agent must achieve" },
                        "context": { "type": "string", "description": "only what it needs to know; it does not see your conversation" },
                        "artifacts": { "type": "array", "items": { "type": "string" }, "description": "art:<id> of artifacts it may use" },
                        "wait": { "type": "boolean", "description": "wait for the result (default true); false returns the task id at once" }
                    }
                })),
            );
            v.push(
                ActionDescriptor::new(
                    WAIT,
                    "wait for delegated tasks and get their results",
                    RiskTier::Observe,
                )
                .volatile()
                .with_input_schema(json!({
                    "type": "object",
                    "required": ["tasks"],
                    "properties": { "tasks": { "type": "array", "items": { "type": "string" } } }
                })),
            );
        }
        if !self.contacts.is_empty() {
            v.push(
                ActionDescriptor::new(
                    MESSAGE,
                    "send a short message to another agent",
                    RiskTier::Observe,
                )
                .volatile()
                .with_input_schema(json!({
                    "type": "object",
                    "required": ["to", "text"],
                    "properties": {
                        "to": { "type": "string", "enum": self.contacts },
                        "text": { "type": "string" }
                    }
                })),
            );
        }
        v
    }

    async fn run(
        &self,
        action: &str,
        input: Value,
        _ctx: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        let orch = self
            .orch
            .upgrade()
            .ok_or_else(|| AdapterError::Tool("the orchestrator is gone".into()))?;
        let s = |k: &str| input.get(k).and_then(Value::as_str).unwrap_or("").trim();
        match action {
            DELEGATE => {
                let to = s("to");
                if !self.delegates.iter().any(|(n, _)| n == to) {
                    return Err(AdapterError::bad_input(
                        action,
                        format!("you may not delegate to `{to}`"),
                    ));
                }
                let artifacts: Vec<String> = input
                    .get("artifacts")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(|x| x.trim_start_matches("art:").to_string())
                            .collect()
                    })
                    .unwrap_or_default();
                let spec = TaskSpec::new(s("objective"))
                    .context(s("context"))
                    .artifacts(artifacts);
                if spec.objective.is_empty() {
                    return Err(AdapterError::bad_input(action, "an objective is required"));
                }
                let wait = input.get("wait").and_then(Value::as_bool).unwrap_or(true);
                let id = orch
                    .delegate(&self.agent, &self.task, to, spec)
                    .map_err(|e| AdapterError::Tool(e.to_string()))?;
                if !wait {
                    return Ok(ActionOutput::json(
                        json!({ "task": id, "agent": to, "state": "queued" }),
                    ));
                }
                let done = Inner::wait_children(&orch, &self.task, std::slice::from_ref(&id)).await;
                Ok(ActionOutput::json(result_json(&done[0])))
            }
            WAIT => {
                let ids: Vec<String> = input
                    .get("tasks")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                let mine: Vec<String> = ids
                    .into_iter()
                    .filter(|id| orch.is_child(&self.task, id))
                    .collect();
                if mine.is_empty() {
                    return Err(AdapterError::bad_input(
                        action,
                        "name tasks you delegated from this task",
                    ));
                }
                let done = Inner::wait_children(&orch, &self.task, &mine).await;
                Ok(ActionOutput::json(json!({
                    "tasks": done.iter().map(result_json).collect::<Vec<_>>()
                })))
            }
            MESSAGE => {
                let to = s("to");
                if !self.contacts.iter().any(|c| c == to) {
                    return Err(AdapterError::bad_input(
                        action,
                        format!("you may not message `{to}`"),
                    ));
                }
                let delivered = orch
                    .send(&self.agent, to, s("text"), Some(&self.task))
                    .map_err(|e| AdapterError::Tool(e.to_string()))?;
                Ok(ActionOutput::json(json!({
                    "to": to,
                    "delivered": if delivered { "into its running task" } else { "kept for its next task" }
                })))
            }
            other => Err(AdapterError::UnknownAction(other.to_string())),
        }
    }
}

/// The tools of the person's conversation in an app that runs several agents. Built for one
/// turn, so the agents it names are the current ones.
pub(crate) struct ChatTools {
    pub orch: Orchestrator,
    /// The conversation (session id): what it schedules posts each run's result there.
    pub chat: Option<String>,
    /// The person's IANA time zone, when the client said it; else the machine's.
    pub zone: Option<String>,
}

/// Schedules the chat tools' descriptions list; the rest are counted.
const SCHEDULES_LISTED: usize = 10;

impl ChatTools {
    fn roster(&self) -> String {
        let live: Vec<String> = self
            .orch
            .agents()
            .into_iter()
            .filter(|a| a.state != AgentState::Terminated)
            .map(|a| match a.spec.role.trim() {
                "" => a.name,
                r => format!("{} ({r})", a.name),
            })
            .collect();
        if live.is_empty() {
            "There are no agents yet; create one first.".into()
        } else {
            format!("Agents: {}.", live.join("; "))
        }
    }

    /// The schedules kept now, for the schedule tools' descriptions: when, the next run, how
    /// the last one went.
    fn scheduled(&self) -> String {
        let all = self.orch.schedules();
        if all.is_empty() {
            return "Nothing is scheduled.".into();
        }
        let mut list: Vec<String> = all
            .iter()
            .take(SCHEDULES_LISTED)
            .map(|s| {
                let state = self.orch.schedule_state(s);
                let next = match s.next_label() {
                    Some(n) => format!(", next {n}"),
                    None => String::new(),
                };
                format!(
                    "{} ({}, {}{next}, {}): {}",
                    s.id,
                    s.agent,
                    s.says,
                    state.as_str(),
                    crate::brief::clip(&s.task.objective, 60)
                )
            })
            .collect();
        if all.len() > SCHEDULES_LISTED {
            list.push(format!("and {} more", all.len() - SCHEDULES_LISTED));
        }
        format!("Scheduled: {}.", list.join("; "))
    }

    /// The zone times are read in: the person's, else the machine's.
    fn zone(&self) -> Option<String> {
        self.zone.clone().or_else(crate::schedule::system_zone)
    }

    /// Today's date, so "tomorrow at 9" and "next Thursday" can be resolved. The date only: a
    /// description that changed every minute would spoil the prompt cache.
    fn now_line(&self) -> String {
        let zone = self
            .zone()
            .and_then(|z| jiff::tz::TimeZone::get(&z).ok())
            .unwrap_or_else(jiff::tz::TimeZone::system);
        let now = jiff::Timestamp::now().to_zoned(zone.clone());
        format!(
            "Today: {} ({}).",
            now.strftime("%a %Y-%m-%d"),
            zone.iana_name().unwrap_or("local time")
        )
    }

    /// A free agent name for a schedule's own agent, from its objective: `recount-stock`.
    fn name_for(&self, objective: &str) -> String {
        let words: Vec<String> = objective
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() > 2)
            .take(3)
            .map(str::to_lowercase)
            .collect();
        let base = match words.join("-") {
            w if w.is_empty() => "scheduled".to_string(),
            w => w
                .chars()
                .take(24)
                .collect::<String>()
                .trim_end_matches('-')
                .to_string(),
        };
        let mut name = base.clone();
        let mut n = 2;
        while self.orch.agent(&name).is_some() {
            name = format!("{base}-{n}");
            n += 1;
        }
        name
    }

    fn live(&self, name: &str) -> bool {
        self.orch
            .agent(name)
            .is_some_and(|a| a.state != AgentState::Terminated)
    }
}

#[async_trait::async_trait]
impl Adapter for ChatTools {
    fn name(&self) -> &str {
        "agents"
    }

    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![
            ActionDescriptor::new(
                DELEGATE,
                format!(
                    "hand a self-contained task to an agent and get its result. Do quick work yourself; hand off work that is long, can run in parallel, or fits an agent's role. {}",
                    self.roster()
                ),
                RiskTier::Observe,
            )
            .volatile()
            .with_input_schema(json!({
                "type": "object",
                "required": ["to", "objective"],
                "properties": {
                    "to": { "type": "string", "description": "the agent's name" },
                    "objective": { "type": "string", "description": "what the agent must achieve" },
                    "context": { "type": "string", "description": "only what it needs to know; it does not see this conversation" },
                    "item": { "type": "string", "description": "the board item (w-…) this task works on; the item then follows the task" },
                    "wait": { "type": "boolean", "description": if self.chat.is_some() { "wait for the result (default true); false returns at once, the task runs on and its result is posted in this chat" } else { "wait for the result (default true); false returns at once and the task runs on" } }
                }
            })),
            ActionDescriptor::new(
                WAIT,
                "wait for tasks handed off with wait: false and get their results",
                RiskTier::Observe,
            )
            .volatile()
            .with_input_schema(json!({
                "type": "object",
                "required": ["tasks"],
                "properties": { "tasks": { "type": "array", "items": { "type": "string" } } }
            })),
            ActionDescriptor::new(
                CREATE,
                "create an agent when the work needs a role no agent has. It keeps its own context between tasks; actions that change anything still ask the person",
                RiskTier::Observe,
            )
            .volatile()
            .with_input_schema(json!({
                "type": "object",
                "required": ["name", "role"],
                "properties": {
                    "name": { "type": "string", "description": "short, lowercase, e.g. reviewer" },
                    "role": { "type": "string", "description": "one line on what it does" },
                    "instructions": { "type": "string", "description": "how it works, as a system prompt" },
                    "skills": { "type": "array", "items": { "type": "string" }, "description": "skills it may read, by name from the SKILLS list" },
                    "capabilities": {
                        "type": "array", "items": { "type": "string" },
                        "description": format!("what it may use, only what its role needs; default: all. One of: {}", self.orch.capabilities().join(", "))
                    }
                }
            })),
            // Standing work spends model time with nobody watching: the person agrees to it,
            // each time. No earlier "always" covers making or changing one: text the chat read
            // (a page, a mail) must never set up work that runs unattended.
            ActionDescriptor::new(
                SCHEDULE,
                format!(
                    "do work on its own at set times (every weekday at 09:00, the 1st of each month, every 30 minutes) or once later (tomorrow at 9, in 2 hours). Each run is a new task{}. With `schedule`, change, resume or run now one of those listed under {UNSCHEDULE}. {}",
                    if self.chat.is_some() { "; its result is posted in this chat" } else { "" },
                    self.now_line(),
                ),
                RiskTier::Mutate,
            )
            .volatile()
            .ask_each_time()
            .with_input_schema(json!({
                "type": "object",
                "properties": {
                    "schedule": { "type": "string", "description": "id (s-…) of one to change; leave out for a new one" },
                    "to": { "type": "string", "description": "an agent; default: one of its own" },
                    "objective": { "type": "string", "description": "what each run does, complete: it does not see this chat (files, places, what to leave)" },
                    "context": { "type": "string" },
                    "at": { "type": "string", "description": "HH:MM" },
                    "days": { "type": "array", "items": { "type": "string" }, "description": "with at: mon..sun, weekdays, weekends" },
                    "date": { "type": "string", "description": "with at: YYYY-MM-DD, one run" },
                    "every_minutes": { "type": "integer", "description": "instead of at; 5 or more" },
                    "cron": { "type": "string", "description": "instead of at: minute hour day month weekday" },
                    "tz": { "type": "string", "description": "IANA zone, only if the person names another" },
                    "once": { "type": "boolean" },
                    "if_missed": { "type": "string", "enum": ["once", "skip"], "description": "a run missed while off: run once when back (default) or skip" },
                    "overlap": { "type": "boolean", "description": "run even while the last one is going" },
                    "resume": { "type": "boolean" },
                    "run_now": { "type": "boolean" }
                }
            })),
            ActionDescriptor::new(
                UNSCHEDULE,
                format!("stop a scheduled task, or pause it to resume later. Runs already done keep their results. {}", self.scheduled()),
                RiskTier::Observe,
            )
            .volatile()
            .with_input_schema(json!({
                "type": "object",
                "required": ["schedule"],
                "properties": {
                    "schedule": { "type": "string", "description": "its id, s-…" },
                    "pause": { "type": "boolean", "description": "keep it, run nothing until resumed (default false: stop it)" }
                }
            })),
        ]
        .into_iter()
        .chain(crate::automate_tools::descriptors(&self.orch, self.chat.is_some()))
        .collect()
    }

    /// The work a stopped turn of this chat handed off, for the turn that continues it.
    fn continued(&self, works: &[String]) -> Vec<String> {
        self.orch
            .tasks_of_work(works)
            .into_iter()
            .map(|t| {
                let objective = crate::brief::clip(&t.objective, 100);
                let state = if t.state.is_final() {
                    match (&t.result, &t.error) {
                        (_, Some(e)) => {
                            format!("{}: {}", t.state.as_str(), crate::brief::clip(e, 120))
                        }
                        (Some(r), _) => format!("finished: {}", crate::brief::clip(r, 160)),
                        _ => t.state.as_str().to_string(),
                    }
                } else {
                    format!(
                        "{}, still under way; its result is posted in this chat",
                        t.state.as_str()
                    )
                };
                format!(
                    "handed off to {} (task {}): {objective} ({state})",
                    t.owner, t.id
                )
            })
            .collect()
    }

    async fn run(
        &self,
        action: &str,
        input: Value,
        ctx: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        let s = |k: &str| input.get(k).and_then(Value::as_str).unwrap_or("").trim();
        let fail = |e: crate::OrchError| AdapterError::Tool(e.to_string());
        match action {
            DELEGATE => {
                let to = s("to");
                if !self.live(to) {
                    return Err(AdapterError::bad_input(
                        action,
                        format!("no agent named `{to}`. {}", self.roster()),
                    ));
                }
                let mut spec = TaskSpec::new(s("objective")).context(s("context"));
                if spec.objective.is_empty() {
                    return Err(AdapterError::bad_input(action, "an objective is required"));
                }
                if !s("item").is_empty() {
                    spec = spec.item(s("item"));
                }
                // Its result comes back to this chat even when this turn is gone (a restart).
                if let Some(chat) = &self.chat {
                    spec = spec.report_to(chat);
                }
                // A turn that stopped handed work to this agent and it is still under way: the
                // turn continuing it takes that up, once, instead of handing it off twice. The
                // model sees what it was; a second hand-off to the agent in this turn is new.
                let taken = ctx.work.as_deref().and_then(|w| {
                    self.orch
                        .take_up(to, &ctx.continues, w)
                        .map(|t| (t.id, t.objective))
                });
                if let Some(w) = &ctx.work {
                    spec = spec.work(w);
                }
                let (id, joined) = match taken {
                    Some((id, objective)) => (id, Some(objective)),
                    None => (
                        self.orch
                            .submit(to, spec)
                            .map_err(|e| AdapterError::bad_input(action, e.to_string()))?,
                        None,
                    ),
                };
                // In other words than before, it may be other work: say what is under way, and
                // do not wait for it. The model hands off again if it wants something else.
                let wait = input.get("wait").and_then(Value::as_bool).unwrap_or(true)
                    && joined.as_deref().is_none_or(|o| o == s("objective"));
                let mark = |mut v: Value| {
                    if let Some(o) = &joined {
                        v["taken_up"] = json!(format!(
                            "this is the hand-off made before the interruption, still under way: {o}. Hand off again only for different work"
                        ));
                    }
                    v
                };
                if !wait {
                    let state = self.orch.task(&id).map(|t| t.state.as_str().to_string());
                    let mut v = json!({ "task": id, "agent": to, "state": state.unwrap_or_else(|| "queued".into()) });
                    if self.chat.is_some() {
                        v["result"] = json!("posted in this chat when it ends");
                    }
                    return Ok(ActionOutput::json(mark(v)));
                }
                let done = self
                    .orch
                    .wait_in(&id, self.chat.as_deref())
                    .await
                    .map_err(fail)?;
                Ok(ActionOutput::json(mark(result_json(&done))))
            }
            WAIT => {
                let ids: Vec<&str> = input
                    .get("tasks")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default();
                if ids.is_empty() {
                    return Err(AdapterError::bad_input(
                        action,
                        "name the tasks to wait for",
                    ));
                }
                let mut done = Vec::new();
                for id in ids {
                    let rec = self
                        .orch
                        .wait_in(id, self.chat.as_deref())
                        .await
                        .map_err(fail)?;
                    done.push(result_json(&rec));
                }
                Ok(ActionOutput::json(json!({ "tasks": done })))
            }
            CREATE => {
                let name = s("name").to_lowercase().replace(' ', "-");
                if self.live(&name) {
                    return Err(AdapterError::bad_input(
                        action,
                        format!("`{name}` exists; hand it the task with {DELEGATE}"),
                    ));
                }
                let skills: Vec<String> = input
                    .get("skills")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                let known = self.orch.capabilities();
                let mut tools: Vec<String> = Vec::new();
                for c in input
                    .get("capabilities")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                {
                    let c = c.trim().trim_end_matches(".*").to_lowercase();
                    if !known.contains(&c) {
                        return Err(AdapterError::bad_input(
                            action,
                            format!("no capability `{c}`; one of: {}", known.join(", ")),
                        ));
                    }
                    tools.push(format!("{c}.*"));
                }
                let mut spec = AgentSpec::new(&name)
                    .role(s("role"))
                    .instructions(s("instructions"))
                    .skills(skills);
                if !tools.is_empty() {
                    spec = spec.tools(tools);
                }
                let a = self.orch.spawn(spec).await.map_err(fail)?;
                Ok(ActionOutput::json(
                    json!({ "agent": a.name, "role": a.spec.role, "tools": a.spec.tools }),
                ))
            }
            SCHEDULE => {
                let bad = |e: crate::OrchError| AdapterError::bad_input(action, e.to_string());
                let id = s("schedule");
                // A new schedule's times are the person's; a change keeps the schedule's zone
                // unless it names one.
                let when = when_of(&input, if id.is_empty() { self.zone() } else { None });
                let flag = |k: &str| input.get(k).and_then(Value::as_bool);
                let mut opts = crate::ScheduleChange {
                    if_missed: match s("if_missed") {
                        "skip" => Some(crate::IfMissed::Skip),
                        "once" => Some(crate::IfMissed::Once),
                        _ => None,
                    },
                    overlap: flag("overlap"),
                    ..Default::default()
                };
                if !id.is_empty() {
                    opts.objective = Some(s("objective").to_string()).filter(|o| !o.is_empty());
                    opts.context = input
                        .get("context")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    opts.when = when;
                    let mut rec = self.orch.change_schedule(id, opts).map_err(|e| {
                        AdapterError::bad_input(action, format!("{e}. {}", self.scheduled()))
                    })?;
                    if flag("resume") == Some(true) && rec.paused {
                        rec = self.orch.resume_schedule(id).map_err(bad)?;
                    }
                    let mut v = schedule_json(&rec);
                    if flag("run_now") == Some(true) {
                        v["run"] = json!(self.orch.run_schedule_now(id).map_err(bad)?);
                    }
                    v["state"] = json!(self.orch.schedule_state(&rec).as_str());
                    return Ok(ActionOutput::json(v));
                }
                let Some(when) = when else {
                    return Err(AdapterError::bad_input(
                        action,
                        "say when: at (HH:MM, with days or a date), every_minutes, or cron",
                    ));
                };
                let objective = s("objective");
                if objective.is_empty() {
                    return Err(AdapterError::bad_input(action, "an objective is required"));
                }
                let mut to = s("to").to_string();
                if !to.is_empty() && !self.live(&to) {
                    return Err(AdapterError::bad_input(
                        action,
                        format!("no agent named `{to}`. {}", self.roster()),
                    ));
                }
                // Checked before an agent is made for it.
                let checked = when
                    .normalized()
                    .map_err(|e| AdapterError::bad_input(action, e))?;
                if checked.next_after(crate::record::now_ms()).is_none() {
                    return Err(AdapterError::bad_input(
                        action,
                        format!(
                            "{} has passed; give a time still to come",
                            checked.describe()
                        ),
                    ));
                }
                if to.is_empty() {
                    // Its own agent: what it carries from run to run is this schedule's alone.
                    to = self.name_for(objective);
                    let spec = AgentSpec::new(&to).role(format!(
                        "does a scheduled task: {}",
                        crate::brief::clip(objective, 80)
                    ));
                    self.orch.spawn(spec).await.map_err(fail)?;
                }
                let mut task = TaskSpec::new(objective).context(s("context"));
                if let Some(chat) = &self.chat {
                    task = task.report_to(chat);
                }
                let own = s("to").is_empty();
                let mut rec = self
                    .orch
                    .schedule_with(&to, task, when, opts)
                    .map_err(bad)?;
                if own {
                    self.orch.own_agent(&rec.id);
                    rec.own_agent = true;
                }
                Ok(ActionOutput::json(schedule_json(&rec)))
            }
            UNSCHEDULE => {
                let id = s("schedule");
                let pause = input.get("pause").and_then(Value::as_bool) == Some(true);
                let done = if pause {
                    self.orch.pause_schedule(id)
                } else {
                    self.orch.stop_schedule(id).await
                };
                let rec = done.map_err(|e| {
                    AdapterError::bad_input(action, format!("{e}. {}", self.scheduled()))
                })?;
                let mut v = schedule_json(&rec);
                v[if pause { "paused" } else { "stopped" }] = json!(true);
                Ok(ActionOutput::json(v))
            }
            crate::automate_tools::AUTOMATE | crate::automate_tools::PAUSE_AUTOMATION => {
                let v = crate::automate_tools::run(
                    &self.orch,
                    self.chat.as_deref(),
                    |o| self.name_for(o),
                    action,
                    &input,
                )
                .await?;
                Ok(ActionOutput::json(v))
            }
            other => Err(AdapterError::UnknownAction(other.to_string())),
        }
    }
}

/// The project's board for the person's conversation: read it, add items, change them. Its
/// changes are the chat's (`by: chat`).
pub(crate) struct BoardTools {
    pub orch: Orchestrator,
}

#[async_trait::async_trait]
impl Adapter for BoardTools {
    fn name(&self) -> &str {
        "board"
    }

    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![
            ActionDescriptor::new(
                BOARD_LIST,
                format!(
                    "read the project's board: the work planned, in progress and done, kept across chats and sessions. Read it before planning or continuing project work. {}",
                    board::summary(&self.orch.board())
                ),
                RiskTier::Observe,
            )
            .volatile()
            .with_input_schema(json!({ "type": "object", "properties": {} })),
            ActionDescriptor::new(
                BOARD_ADD,
                "add items to the project's board: pieces of project work worth tracking (a deliverable, a step that takes a task), not every small action. Hand one to an agent with agents.delegate and its item id",
                RiskTier::Observe,
            )
            .with_input_schema(json!({
                "type": "object",
                "required": ["items"],
                "properties": {
                    "items": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "required": ["title"],
                            "properties": {
                                "title": { "type": "string", "description": "short, what is done when it is done" },
                                "note": { "type": "string" },
                                "owner": { "type": "string", "description": "an agent, or user for the person" }
                            }
                        }
                    }
                }
            })),
            ActionDescriptor::new(
                BOARD_UPDATE,
                "change a board item: its state, what blocks it (with owner user when only the person can unblock it; empty clears it), owner, note or title",
                RiskTier::Observe,
            )
            .with_input_schema(json!({
                "type": "object",
                "required": ["item"],
                "properties": {
                    "item": { "type": "string", "description": "w-…" },
                    "state": { "type": "string", "enum": ["todo", "doing", "done"] },
                    "blocked": { "type": "string" },
                    "owner": { "type": "string" },
                    "note": { "type": "string" },
                    "title": { "type": "string" }
                }
            })),
        ]
    }

    async fn run(
        &self,
        action: &str,
        input: Value,
        _ctx: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        let opt = |v: &Value, k: &str| {
            v.get(k)
                .and_then(Value::as_str)
                .map(|x| x.trim().to_string())
        };
        let bad = |e: crate::OrchError| AdapterError::bad_input(action, e.to_string());
        match action {
            BOARD_LIST => {
                let all = self.orch.board();
                let open: Vec<&WorkItem> =
                    all.iter().filter(|w| w.state != WorkState::Done).collect();
                let done: Vec<&WorkItem> =
                    all.iter().filter(|w| w.state == WorkState::Done).collect();
                Ok(ActionOutput::json(json!({
                    "summary": board::summary(&all),
                    "items": open.iter().take(BOARD_LISTED).map(|w| item_json(w)).collect::<Vec<_>>(),
                    "more_open": open.len().saturating_sub(BOARD_LISTED),
                    "done": done.iter().take(DONE_LISTED).map(|w| item_json(w)).collect::<Vec<_>>(),
                    "more_done": done.len().saturating_sub(DONE_LISTED),
                })))
            }
            BOARD_ADD => {
                let items = input
                    .get("items")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                if items.is_empty() {
                    return Err(AdapterError::bad_input(action, "name at least one item"));
                }
                let mut added = Vec::new();
                for it in &items {
                    let title = opt(it, "title").unwrap_or_default();
                    let change = WorkChange {
                        note: opt(it, "note").filter(|n| !n.is_empty()),
                        owner: opt(it, "owner").filter(|n| !n.is_empty()),
                        ..Default::default()
                    };
                    added.push(item_json(
                        &self.orch.add_work(CHAT, &title, change).map_err(bad)?,
                    ));
                }
                Ok(ActionOutput::json(json!({ "added": added })))
            }
            BOARD_UPDATE => {
                let id = opt(&input, "item").unwrap_or_default();
                let state = match opt(&input, "state").filter(|x| !x.is_empty()) {
                    Some(x) => Some(WorkState::parse(&x).ok_or_else(|| {
                        AdapterError::bad_input(action, "state is todo, doing or done")
                    })?),
                    None => None,
                };
                let change = WorkChange {
                    title: opt(&input, "title").filter(|n| !n.is_empty()),
                    note: opt(&input, "note"),
                    state,
                    owner: opt(&input, "owner"),
                    blocked: opt(&input, "blocked"),
                    before: None,
                };
                let w = self.orch.change_work(CHAT, &id, change).map_err(bad)?;
                Ok(ActionOutput::json(json!({ "updated": item_json(&w) })))
            }
            other => Err(AdapterError::UnknownAction(other.to_string())),
        }
    }
}

fn item_json(w: &WorkItem) -> Value {
    let mut v = json!({ "item": w.id, "title": w.title, "state": w.state.as_str() });
    if let Some(o) = &w.owner {
        v["owner"] = json!(o);
    }
    if let Some(b) = &w.blocked {
        v["blocked"] = json!(b);
    }
    if !w.note.is_empty() {
        v["note"] = json!(crate::brief::clip(&w.note, 300));
    }
    v
}

/// One line per item in an evidence card: `w-1a2b3c4d [doing, writer] Title (blocked: …)`.
fn item_line(v: &Value) -> String {
    let g = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("");
    let mut s = format!("{} [{}", g("item"), g("state"));
    if !g("owner").is_empty() {
        s.push_str(&format!(", {}", g("owner")));
    }
    s.push_str(&format!("] {}", g("title")));
    if !g("blocked").is_empty() {
        s.push_str(&format!(" (blocked: {})", g("blocked")));
    }
    s
}

/// Evidence cards for `board.*` results: one line per item, the item ids as identifiers.
pub(crate) fn board_distiller() -> Distiller {
    Arc::new(|action: &str, out: &Value| {
        let lines = |key: &str| -> Vec<Value> {
            match out.get(key) {
                Some(Value::Array(a)) => a.clone(),
                Some(v @ Value::Object(_)) => vec![v.clone()],
                _ => Vec::new(),
            }
        };
        let ids = |items: &[Value]| -> Vec<String> {
            items
                .iter()
                .filter_map(|v| v.get("item").and_then(Value::as_str).map(str::to_string))
                .collect()
        };
        match action {
            BOARD_LIST => {
                let open = lines("items");
                let done = lines("done");
                let mut s = out
                    .get("summary")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                for v in &open {
                    s.push_str(&format!("\n{}", item_line(v)));
                }
                if let Some(n) = out
                    .get("more_open")
                    .and_then(Value::as_u64)
                    .filter(|n| *n > 0)
                {
                    s.push_str(&format!("\n({n} more open)"));
                }
                for v in &done {
                    s.push_str(&format!("\n{}", item_line(v)));
                }
                let mut all = ids(&open);
                all.extend(ids(&done));
                Some((s, all))
            }
            BOARD_ADD => {
                let added = lines("added");
                let s = added.iter().map(item_line).collect::<Vec<_>>().join("\n");
                let n = match added.len() {
                    1 => "1 item".to_string(),
                    n => format!("{n} items"),
                };
                Some((format!("added {n} to the board:\n{s}"), ids(&added)))
            }
            BOARD_UPDATE => {
                let v = lines("updated");
                Some((
                    format!(
                        "board item now: {}",
                        v.first().map(item_line).unwrap_or_default()
                    ),
                    ids(&v),
                ))
            }
            _ => None,
        }
    })
}

fn schedule_json(s: &crate::ScheduleRecord) -> Value {
    json!({
        "schedule": s.id,
        "agent": s.agent,
        "objective": s.task.objective,
        "when": s.says,
        "tz": s.when.tz,
        "next": s.next_label(),
        "paused": s.paused,
    })
}

/// The `When` an `agents.schedule` input gives, if it gives one; times of day in `zone` unless
/// it names its own.
fn when_of(input: &Value, zone: Option<String>) -> Option<crate::When> {
    let s = |k: &str| {
        input
            .get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    };
    let w = crate::When {
        every_minutes: input
            .get("every_minutes")
            .and_then(Value::as_u64)
            .map(|m| m.min(u32::MAX as u64) as u32),
        at: s("at"),
        days: input
            .get("days")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        date: s("date"),
        cron: s("cron"),
        tz: s("tz").or(zone),
        once: input.get("once").and_then(Value::as_bool).unwrap_or(false),
    };
    (w.every_minutes.is_some() || w.at.is_some() || w.cron.is_some()).then_some(w)
}

fn result_json(t: &TaskRecord) -> Value {
    json!({
        "task": t.id,
        "agent": t.owner,
        "state": t.state.as_str(),
        "result": t.result,
        "error": t.error,
        "artifacts": t.outputs,
        "tokens": t.usage.tokens(),
    })
}

/// Evidence cards for `agents.*` results: the delegated result itself, clipped, with the
/// task and artifact ids as identifiers.
pub(crate) fn distiller() -> Distiller {
    Arc::new(|action: &str, out: &Value| {
        let one = |t: &Value, path: &str| -> (String, Vec<String>) {
            let g = |k: &str| t.get(k).and_then(Value::as_str).unwrap_or("");
            let mut ids = vec![g("task").to_string()];
            let mut line = format!("{} {} task {}", g("agent"), g("state"), g("task"));
            if !g("error").is_empty() {
                line.push_str(&format!(" (error: {})", g("error")));
            }
            if !g("taken_up").is_empty() {
                line.push_str(&format!(" ({})", g("taken_up")));
            }
            if !g("result").is_empty() {
                line.push_str(&format!(
                    ": {}",
                    crate::brief::clip(g("result"), RESULT_CHARS)
                ));
                let n = g("result").chars().count();
                if n > RESULT_CHARS {
                    // Say how to read the rest: a delegator that misses it works from half a result.
                    line.push_str(&format!(
                        "\n  (clipped: {RESULT_CHARS} of {n} characters; get_evidence on this invocation with path \"{path}\" reads all of it)"
                    ));
                }
            }
            for a in t
                .get("artifacts")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let id = a.get("id").and_then(Value::as_str).unwrap_or("");
                let name = a.get("name").and_then(Value::as_str).unwrap_or("");
                line.push_str(&format!(
                    "\n  produced art:{} {name}",
                    crate::brief::short_id(id)
                ));
                ids.push(id.to_string());
            }
            (line, ids)
        };
        match action {
            DELEGATE => Some(one(out, "result")),
            WAIT => {
                let mut lines = Vec::new();
                let mut ids = Vec::new();
                for (n, t) in out
                    .get("tasks")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .enumerate()
                {
                    let (l, i) = one(t, &format!("tasks.{n}.result"));
                    lines.push(l);
                    ids.extend(i);
                }
                Some((lines.join("\n"), ids))
            }
            CREATE => Some((
                format!(
                    "created agent {}: {}",
                    out.get("agent").and_then(Value::as_str).unwrap_or(""),
                    out.get("role").and_then(Value::as_str).unwrap_or("")
                ),
                Vec::new(),
            )),
            SCHEDULE | UNSCHEDULE => {
                let g = |k: &str| out.get(k).and_then(Value::as_str).unwrap_or("");
                let yes = |k: &str| out.get(k).and_then(Value::as_bool) == Some(true);
                let verb = if yes("stopped") {
                    "stopped"
                } else if yes("paused") {
                    "paused"
                } else {
                    "scheduled"
                };
                let mut card = format!(
                    "{verb} {}: {} runs \"{}\" {}",
                    g("schedule"),
                    g("agent"),
                    crate::brief::clip(g("objective"), 120),
                    g("when")
                );
                if !g("next").is_empty() && verb == "scheduled" {
                    card.push_str(&format!(", next {}", g("next")));
                }
                if !g("run").is_empty() {
                    card.push_str(&format!("; started run {}", g("run")));
                }
                Some((card, vec![g("schedule").to_string()]))
            }
            MESSAGE => Some((
                format!(
                    "message to {} {}",
                    out.get("to").and_then(Value::as_str).unwrap_or(""),
                    out.get("delivered").and_then(Value::as_str).unwrap_or("")
                ),
                Vec::new(),
            )),
            _ => None,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clipped_result_says_how_to_read_the_rest() {
        let d = distiller();
        let long = "x".repeat(RESULT_CHARS + 50);
        let (card, ids) = d(
            DELEGATE,
            &json!({ "task": "t-1", "agent": "w", "state": "done", "result": long }),
        )
        .unwrap();
        assert!(card.contains("path \"result\" reads all of it"), "{card}");
        assert_eq!(ids, vec!["t-1"]);
        let (card, _) = d(
            WAIT,
            &json!({ "tasks": [
                { "task": "t-1", "agent": "w", "state": "done", "result": "short" },
                { "task": "t-2", "agent": "w", "state": "done", "result": "y".repeat(RESULT_CHARS + 1) }
            ] }),
        )
        .unwrap();
        assert!(card.contains("path \"tasks.1.result\""), "{card}");
        assert!(!card.contains("tasks.0.result"));
    }
}
