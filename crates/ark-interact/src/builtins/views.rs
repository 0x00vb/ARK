//! Commands that show what the agent sees, knows and spends.

use ark_sdk::prelude::ark_core::{Invocation, RiskTier};

use crate::command::{Command, Reply};
use crate::fmt::{clip, count, millis, tokens};
use crate::interaction::Interaction;
use crate::report::{Cell, Report, Row, Table, Tone};
use crate::trace::{decision_word, CallStatus, CallView, EntryKind, Trace};

pub struct Context;
#[async_trait::async_trait]
impl Command for Context {
    meta!(
        "context",
        "What the model receives, by source, and what ARK kept out"
    );
    async fn run(&self, ix: &mut Interaction, _args: &str) -> anyhow::Result<Reply> {
        Ok(ix.context_view().await?.report().into())
    }
}

pub struct Agents;
#[async_trait::async_trait]
impl Command for Agents {
    meta!(
        "agents",
        "Agents, the tasks they run and delegate, and one task's trace"
    );
    fn usage(&self) -> &str {
        "[task]"
    }
    async fn run(&self, ix: &mut Interaction, args: &str) -> anyhow::Result<Reply> {
        let Some(o) = ix.orchestrator() else {
            return Ok(Report::new().note("This app runs a single agent.").into());
        };
        let g = o.graph();
        let task = args.trim();
        if task.is_empty() {
            return Ok(crate::orchestration::graph_report_with_workers(
                &g,
                ix.labels(),
                ix.ark().workers().map(|workers| workers.as_ref()),
            )
            .into());
        }
        let events = o.task_trace(task)?;
        Ok(crate::orchestration::task_report_with_workers(
            &g,
            task,
            &events,
            ix.ark().workers().map(|workers| workers.as_ref()),
        )
        .into())
    }
}

/// `/agent` and `/task`: change the agents, the same [`Control`](crate::orchestration::Control)s
/// the window sends.
pub struct AgentCmd;
#[async_trait::async_trait]
impl Command for AgentCmd {
    meta!(
        "agent",
        "Add, pause, resume, end or message an agent: /agent new <name> [model]"
    );
    fn usage(&self) -> &str {
        "new|pause|resume|end|message <name> …"
    }
    async fn run(&self, ix: &mut Interaction, args: &str) -> anyhow::Result<Reply> {
        control_cmd(ix, crate::orchestration::parse_agent(args)).await
    }
}

/// `/schedules`: what agents run on their own, and stopping one.
pub struct Schedules;
#[async_trait::async_trait]
impl Command for Schedules {
    meta!("schedules", "Tasks agents run on their own at set times");
    fn usage(&self) -> &str {
        "[run|pause|resume|stop <schedule>]"
    }
    async fn run(&self, ix: &mut Interaction, args: &str) -> anyhow::Result<Reply> {
        let Some(o) = ix.orchestrator() else {
            return Ok(Report::new().note("This app runs a single agent.").into());
        };
        let args = args.trim();
        if args.is_empty() {
            return Ok(crate::orchestration::schedules_report(o).into());
        }
        use crate::orchestration::Control as C;
        let parsed = match args.split_once(char::is_whitespace) {
            Some((verb, id)) if !id.trim().is_empty() => {
                let schedule = id.trim().to_string();
                match verb {
                    "stop" => Ok(C::Unschedule { schedule }),
                    "pause" => Ok(C::PauseSchedule { schedule }),
                    "resume" => Ok(C::ResumeSchedule { schedule }),
                    "run" => Ok(C::RunSchedule { schedule }),
                    _ => Err(String::new()),
                }
            }
            _ => Err(String::new()),
        }
        .map_err(|_| "usage: /schedules [run|pause|resume|stop <schedule>]".to_string());
        control_cmd(ix, parsed).await
    }
}

/// `/automations`: everything that runs on its own (schedules and event automations), and
/// pausing, resuming, stopping or draining one.
pub struct Automations;
#[async_trait::async_trait]
impl Command for Automations {
    meta!(
        "automations",
        "Work that runs on its own: at set times or when a message arrives"
    );
    fn usage(&self) -> &str {
        "[pause|resume|stop|discard <id>]"
    }
    async fn run(&self, ix: &mut Interaction, args: &str) -> anyhow::Result<Reply> {
        let Some(o) = ix.orchestrator() else {
            return Ok(Report::new().note("This app runs a single agent.").into());
        };
        let args = args.trim();
        if args.is_empty() {
            return Ok(crate::automations::automations_report(o).into());
        }
        use crate::orchestration::Control as C;
        let usage = "usage: /automations [pause|resume|stop|discard <id>]".to_string();
        let parsed = match args.split_once(char::is_whitespace) {
            Some((verb, id)) if !id.trim().is_empty() => {
                let id = id.trim().to_string();
                let schedule = !id.starts_with("a-");
                match (verb, schedule) {
                    ("pause", true) => Ok(C::PauseSchedule { schedule: id }),
                    ("resume", true) => Ok(C::ResumeSchedule { schedule: id }),
                    ("stop", true) => Ok(C::Unschedule { schedule: id }),
                    ("pause", false) => Ok(C::PauseAutomation { automation: id }),
                    ("resume", false) => Ok(C::ResumeAutomation { automation: id }),
                    ("stop", false) => Ok(C::StopAutomation { automation: id }),
                    ("discard", false) => Ok(C::DiscardPending { automation: id }),
                    _ => Err(usage),
                }
            }
            _ => Err(usage),
        };
        control_cmd(ix, parsed).await
    }
}

/// `/board`: the project's work by column, and changing it.
pub struct Board;
#[async_trait::async_trait]
impl Command for Board {
    meta!("board", "The project's work: to do, in progress and done");
    fn usage(&self) -> &str {
        "[add <title> | done <item> | move <item> <column> | give <item> <agent> | remove <item>]"
    }
    async fn run(&self, ix: &mut Interaction, args: &str) -> anyhow::Result<Reply> {
        let Some(o) = ix.orchestrator() else {
            return Ok(Report::new().note("This app runs a single agent.").into());
        };
        if args.trim().is_empty() {
            return Ok(crate::orchestration::board_report(&o.board()).into());
        }
        control_cmd(ix, crate::orchestration::parse_board(args)).await
    }
}

pub struct Waiting;
#[async_trait::async_trait]
impl Command for Waiting {
    meta!(
        "waiting",
        "What waits for you in any project: questions, work that failed, results"
    );
    fn usage(&self) -> &str {
        "[<n> [once|session|always|deny] | clear]"
    }
    async fn run(&self, ix: &mut Interaction, args: &str) -> anyhow::Result<Reply> {
        use crate::attention;
        // This runtime's items and the linked runtimes' (ADR 0043 step 4); each is opened,
        // cleared and answered on its own runtime.
        let links = ix.links().clone();
        let items = attention::everywhere(ix.engine(), ix.labels(), &links);
        let mut words = args.split_whitespace();
        let (first, second) = (words.next(), words.next());
        match first {
            None => {
                ix.waiting = items
                    .iter()
                    .map(|i| (i.runtime.clone(), i.id.clone()))
                    .collect();
                Ok(attention::report_with(&items, &attention::link_notes(&links, true)).into())
            }
            Some("clear") => {
                let mut n = 0;
                // A runtime that does not answer is not waited on item by item: its items stay.
                let mut failed: Vec<String> = attention::away(&links);
                for i in items.iter().filter(|i| !i.is_question()) {
                    let done = match &i.runtime {
                        None => attention::clear(ix.engine(), i).is_ok(),
                        Some(r) if failed.contains(r) => false,
                        Some(r) => {
                            let ok = links.clear(r, &i.id).await.is_ok();
                            if !ok {
                                failed.push(r.clone());
                            }
                            ok
                        }
                    };
                    n += usize::from(done);
                }
                ix.waiting.clear();
                let mut r = Report::new()
                    .success(crate::fmt::count(n as u64, "item"))
                    .note("Cleared. The work stays in its chat and in /agents.");
                for name in failed.iter().filter(|f| {
                    items
                        .iter()
                        .any(|i| i.runtime.as_ref() == Some(*f) && !i.is_question())
                }) {
                    r = r.warning(format!("{name} is not answering: what waits there stays."));
                }
                Ok(r.into())
            }
            Some(n) => {
                // The number of the list the person saw, never of a newer one.
                let Some(key) = n
                    .parse::<usize>()
                    .ok()
                    .and_then(|n| ix.waiting.get(n.wrapping_sub(1)))
                    .cloned()
                else {
                    // Numbers follow the list shown last, so they never shift onto other work.
                    let say = if ix.waiting.is_empty() {
                        "Numbers follow the list. /waiting shows it.".to_string()
                    } else {
                        format!("No item {n}. /waiting lists what waits now.")
                    };
                    return Ok(Report::new().warning(say).into());
                };
                let Some(item) = items
                    .into_iter()
                    .find(|i| i.runtime == key.0 && i.id == key.1)
                else {
                    return Ok(Report::new()
                        .note(format!(
                            "Item {n} is no longer waiting. /waiting lists what waits now."
                        ))
                        .into());
                };
                let n: usize = n.parse().unwrap_or(0);
                match (second, &item.runtime) {
                    (None, None) => {
                        let r = attention::opened(ix.engine(), &item, n);
                        if !item.is_question() {
                            attention::clear(ix.engine(), &item)?;
                        }
                        Ok(r.into())
                    }
                    // On another runtime: its words from there; its chat is there.
                    (None, Some(runtime)) => {
                        let opened = links.open(runtime, &item.id).await?;
                        let mut r =
                            Report::titled(item.title.clone()).field("where", item.place.clone());
                        if let Some(t) = opened["text"].as_str() {
                            r = r.text(t.to_string());
                        }
                        if item.is_question() {
                            r = r.note(format!(
                                "Answer here: /waiting {n} once|session|always|deny"
                            ));
                        }
                        Ok(r.note(format!("It is on {runtime}.")).into())
                    }
                    (Some(word), runtime) if item.is_question() => {
                        let decision = match crate::orchestration::decision(word) {
                            Ok(d) => d,
                            Err(e) => return Ok(Report::new().warning(e).into()),
                        };
                        match runtime {
                            None => ix.orchestrators().answer(&item.id, decision)?,
                            Some(r) => links.answer(r, &item.id, decision).await?,
                        }
                        Ok(Report::new()
                            .success(format!("Answered: {}", item.title))
                            .into())
                    }
                    (Some(_), _) => Ok(Report::new()
                        .warning(format!(
                            "Item {n} is not a question. /waiting {n} opens it."
                        ))
                        .into()),
                }
            }
        }
    }
}

pub struct TaskCmd;
#[async_trait::async_trait]
impl Command for TaskCmd {
    meta!(
        "task",
        "Hand an agent a task, cancel, retry or answer one: /task <agent> <objective>"
    );
    fn usage(&self) -> &str {
        "<agent> <objective> | cancel|retry <task> | answer <task> once|session|always|deny"
    }
    async fn run(&self, ix: &mut Interaction, args: &str) -> anyhow::Result<Reply> {
        control_cmd(ix, crate::orchestration::parse_task(args)).await
    }
}

async fn control_cmd(
    ix: &mut Interaction,
    parsed: Result<crate::orchestration::Control, String>,
) -> anyhow::Result<Reply> {
    let Some(o) = ix.orchestrator() else {
        return Ok(Report::new().note("This app runs a single agent.").into());
    };
    let c = match parsed {
        Ok(c) => c,
        Err(usage) => return Ok(Report::new().warning(usage).into()),
    };
    Ok(crate::orchestration::control(o, c).await?.report().into())
}

pub struct Usage;
#[async_trait::async_trait]
impl Command for Usage {
    meta!("usage", "Tokens, model calls and tools used this session");
    async fn run(&self, ix: &mut Interaction, _args: &str) -> anyhow::Result<Reply> {
        Ok(ix.usage_view().report().into())
    }
}

pub struct State;
#[async_trait::async_trait]
impl Command for State {
    meta!("state", "The working state the agent carries between turns");
    async fn run(&self, ix: &mut Interaction, _args: &str) -> anyhow::Result<Reply> {
        Ok(ix.state_view()?.report().into())
    }
}

pub struct Tools;
#[async_trait::async_trait]
impl Command for Tools {
    meta!("tools", "The tools this agent can use");
    async fn run(&self, ix: &mut Interaction, _args: &str) -> anyhow::Result<Reply> {
        let mut actions = ix.ark().tools().actions();
        actions.sort_by(|a, b| a.name.cmp(&b.name));
        let core = ix.ark().profile().core_actions.clone();
        let mut t = Table::plain(3);
        for d in &actions {
            let (risk, tone) = match d.risk {
                RiskTier::Read => ("read", Tone::Success),
                RiskTier::Observe => ("observe", Tone::Accent),
                RiskTier::Mutate => ("changes", Tone::Warning),
            };
            let name = if core.contains(&d.name) {
                format!("{} ·preloaded", d.name)
            } else {
                d.name.clone()
            };
            t.push(Row {
                cells: vec![
                    Cell::new(risk, tone),
                    Cell::new(name, Tone::Normal),
                    Cell::new(d.summary.clone(), Tone::Muted),
                ],
                tone: Tone::Normal,
                command: None,
            });
        }
        Ok(Report::new()
            .table(t)
            .note(format!(
                "{} actions. `changes` asks before running. Schemas load on demand; the model sees a one-line index.",
                actions.len()
            ))
            .into())
    }
}

pub struct Connect;
#[async_trait::async_trait]
impl Command for Connect {
    meta!(
        "connect",
        "What ARK can use (Web, Mail, Calendar, …); connect one with /connect <name>"
    );
    fn usage(&self) -> &str {
        "[capability]"
    }
    async fn run(&self, ix: &mut Interaction, args: &str) -> anyhow::Result<Reply> {
        let which = args.trim();
        let mut r = Report::new();
        if crate::capabilities::switchable(ix.ark().computer(), which) {
            crate::capabilities::turn(ix.ark().computer(), which, true)?;
            r = r.success("The Computer is on. Agents still ask before each step.");
        } else if !which.is_empty() {
            let url = crate::capabilities::connect(ix.ark().connections(), which).await?;
            r = if crate::capabilities::open_in_browser(&url) {
                r.success("Opened the sign-in in your browser. Once you allow it there, the capability is ready for the next turn.")
            } else {
                r.text(format!("Open this page to sign in, then come back:\n{url}"))
            };
        }
        r.blocks
            .extend(crate::capabilities::report(&crate::capabilities::list(ix.ark())).blocks);
        Ok(r.into())
    }
}

pub struct Grants;
#[async_trait::async_trait]
impl Command for Grants {
    meta!(
        "grants",
        "What agents may do without asking; remove one with /grants remove <name>"
    );
    fn usage(&self) -> &str {
        "[remove <name>]"
    }
    async fn run(&self, ix: &mut Interaction, args: &str) -> anyhow::Result<Reply> {
        let (verb, rest) = args.trim().split_once(' ').unwrap_or((args.trim(), ""));
        let mut r = Report::new();
        match verb {
            "" | "list" => {}
            "remove" if !rest.trim().is_empty() => {
                r = if ix.remove_grant(rest) {
                    r.success(format!(
                        "Removed. Agents ask again before `{}`.",
                        rest.trim()
                    ))
                } else {
                    r.warning(format!(
                        "No lasting grant named `{}` in this project.",
                        rest.trim()
                    ))
                };
            }
            _ => anyhow::bail!("usage: /grants {}", self.usage()),
        }
        let (session, lasting) = ix.grants_view();
        if session.is_empty() && lasting.is_empty() {
            return Ok(r
                .note("Nothing runs without asking. Approvals offer \"for this session\" and \"always in this project\".")
                .into());
        }
        if !lasting.is_empty() {
            let mut t = Table::plain(3);
            for g in &lasting {
                t.push(crate::report::Row {
                    cells: vec![
                        Cell::new(g.class.clone(), Tone::Normal),
                        Cell::new(g.preview.clone(), Tone::Muted),
                        Cell::new(
                            g.granted_at.get(..10).unwrap_or("").to_string(),
                            Tone::Muted,
                        ),
                    ],
                    tone: Tone::Normal,
                    command: Some(format!("/grants remove {}", g.class)),
                });
            }
            r = r.heading("Always in this project").table(t);
        }
        let only_session: Vec<&String> = session
            .iter()
            .filter(|c| !lasting.iter().any(|g| &g.class == *c))
            .collect();
        if !only_session.is_empty() {
            r = r.heading("For this session").text(
                only_session
                    .iter()
                    .map(|c| c.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            );
        }
        Ok(r.into())
    }
}

pub struct Disconnect;
#[async_trait::async_trait]
impl Command for Disconnect {
    meta!(
        "disconnect",
        "Disconnect an account: revoke ARK's access and forget it"
    );
    fn usage(&self) -> &str {
        "<capability>"
    }
    async fn run(&self, ix: &mut Interaction, args: &str) -> anyhow::Result<Reply> {
        let which = args.trim();
        if which.is_empty() {
            anyhow::bail!("usage: /disconnect {}", self.usage());
        }
        if crate::capabilities::switchable(ix.ark().computer(), which) {
            crate::capabilities::turn(ix.ark().computer(), which, false)?;
            let mut r = Report::new()
                .success("The Computer is off. Agents ask before they use your apps again.");
            r.blocks
                .extend(crate::capabilities::report(&crate::capabilities::list(ix.ark())).blocks);
            return Ok(r.into());
        }
        let account = crate::capabilities::disconnect(ix.ark().connections(), which).await?;
        let mut r = Report::new().success(format!(
            "Disconnected {}. ARK's access is revoked; every capability it served needs connecting again.",
            account.name
        ));
        r.blocks
            .extend(crate::capabilities::report(&crate::capabilities::list(ix.ark())).blocks);
        Ok(r.into())
    }
}

pub struct Plugin;
#[async_trait::async_trait]
impl Command for Plugin {
    meta!(
        "plugin",
        "MCP servers whose tools the agents can use: list, add, remove, restart"
    );
    fn usage(&self) -> &str {
        "[add <name> [KEY=VALUE…] <command> [args…] | remove <name> | restart <name>]"
    }
    async fn run(&self, ix: &mut Interaction, args: &str) -> anyhow::Result<Reply> {
        let engine = ix.ark().shared_engine();
        if !crate::plugins::supported(&engine) {
            return Ok(Report::new().note("This app has no plugins.").into());
        }
        let (verb, rest) = args.trim().split_once(' ').unwrap_or((args.trim(), ""));
        let rest = rest.trim();
        let done = match verb {
            "" | "list" => None,
            "add" => {
                let added = crate::plugins::add_text(engine.clone(), rest).await?;
                let names: Vec<String> = added
                    .iter()
                    .map(|p| format!("{} ({} tools)", p.name, p.tools.len()))
                    .collect();
                Some(format!(
                    "Added {}. Agents may use them from the next turn.",
                    names.join(", ")
                ))
            }
            "remove" if !rest.is_empty() => {
                crate::plugins::remove(engine.clone(), rest.to_string()).await?;
                Some(format!("Removed {rest}."))
            }
            "restart" if !rest.is_empty() => {
                let p = crate::plugins::restart(engine.clone(), rest.to_string()).await?;
                Some(match p.error {
                    None => format!("Restarted {rest}."),
                    Some(e) => format!("{rest} did not start: {e}"),
                })
            }
            _ => anyhow::bail!("usage: /plugin {}", self.usage()),
        };
        let list = crate::plugins::report(&crate::plugins::list(&engine));
        let mut r = match done {
            Some(d) => Report::new().success(d),
            None => Report::new(),
        };
        r.blocks.extend(list.blocks);
        Ok(r.into())
    }
}

pub struct Skills;
#[async_trait::async_trait]
impl Command for Skills {
    meta!(
        "skills",
        "Skills the conversation and agents can read, and where they are"
    );
    async fn run(&self, ix: &mut Interaction, _args: &str) -> anyhow::Result<Reply> {
        let found = crate::skills::list(ix.ark(), &ix.project().id);
        Ok(crate::skills::report(&found, ix.ark().engine(), &ix.project().id).into())
    }
}

pub struct TraceCmd;
#[async_trait::async_trait]
impl Command for TraceCmd {
    meta!(
        "trace",
        "Inspect the last turn: every step, call, and cost. /trace <n> shows call n"
    );
    fn usage(&self) -> &str {
        "[n]"
    }
    async fn run(&self, ix: &mut Interaction, args: &str) -> anyhow::Result<Reply> {
        let Some(trace) = ix.trace(None) else {
            return Ok(Report::new().note("Nothing to trace yet.").into());
        };
        let args = args.trim();
        if args.is_empty() {
            return Ok(trace_report(&trace).into());
        }
        let n: usize = args
            .trim_start_matches('#')
            .parse()
            .map_err(|_| anyhow::anyhow!("usage: /trace <n>"))?;
        let Some(call) = trace.call(n) else {
            anyhow::bail!(
                "no call #{n} in the last turn ({} calls)",
                trace.calls.len()
            );
        };
        let inv = match &call.invocation {
            Some(id) => ix.engine().store.get_invocation(&ix.project().id, id)?,
            None => None,
        };
        Ok(call_report(call, inv.as_ref()).into())
    }
}

/// A turn's timeline as a report.
pub fn trace_report(t: &Trace) -> Report {
    let mut table = Table::plain(3).right(0);
    for e in &t.entries {
        let text = match (e.kind, e.call) {
            (EntryKind::ToolCall, Some(n)) => format!("#{n} {}", e.text),
            _ => e.text.clone(),
        };
        let indent = !matches!(
            e.kind,
            EntryKind::Step | EntryKind::Answer | EntryKind::Cancelled
        );
        table.push(Row {
            cells: vec![
                Cell::new(format!("{:.2}s", e.at_ms as f64 / 1000.0), Tone::Muted),
                Cell::new(if indent { format!("  {text}") } else { text }, e.tone),
                Cell::new(e.detail.clone().unwrap_or_default(), Tone::Muted),
            ],
            tone: Tone::Normal,
            command: e
                .call
                .filter(|_| e.kind == EntryKind::ToolCall)
                .map(|n| format!("/trace {n}")),
        });
    }
    let mut r = Report::titled(format!("Trace · {}", clip(&t.input, 60))).table(table);
    if let Some(e) = &t.error {
        r = r.error(format!("error: {e}"));
    }
    let approx = if t.measured { "" } else { "~" };
    r.note(format!(
        "{} total · {} · {approx}{} in / {approx}{} out · /trace <n> for a call's input and output",
        millis(t.elapsed_ms),
        count(t.model_calls as u64, "model call"),
        tokens(t.prompt_tokens),
        tokens(t.completion_tokens)
    ))
    .data("trace", t)
}

fn status_word(s: CallStatus) -> (&'static str, Tone) {
    match s {
        CallStatus::Running => ("running", Tone::Accent),
        CallStatus::Ok => ("ok", Tone::Success),
        CallStatus::Failed => ("failed", Tone::Error),
        CallStatus::Denied => ("denied by you", Tone::Warning),
        CallStatus::Refused => ("not run: needed approval", Tone::Warning),
        CallStatus::Reused => ("reused an earlier result", Tone::Muted),
        CallStatus::NotRun => ("not run (turn stopped)", Tone::Muted),
    }
}

/// One call in full: status, approval, context contribution, input and output.
pub fn call_report(c: &CallView, inv: Option<&Invocation>) -> Report {
    let (word, tone) = status_word(c.status);
    let mut r = Report::titled(format!("#{} {}", c.n, c.action))
        .field_toned("status", word, tone)
        .field("step", c.step.to_string());
    if let Some(d) = c.duration_ms {
        r = r.field("duration", millis(d.max(0) as u64));
    }
    if let Some(a) = &c.approval {
        let how = if a.remembered {
            "remembered for the session"
        } else {
            "asked"
        };
        r = r.field(
            "approval",
            format!("{} ({}, {how})", decision_word(a.decision), a.class),
        );
    }
    if let Some(n) = c.context_tokens {
        let window = match c.left_window_at {
            Some(s) => format!("; left the window at step {s}"),
            None => String::new(),
        };
        r = r.field(
            "context",
            format!(
                "+{n} tokens (distilled); carried by {} later request(s){window}",
                c.seen_by_requests
            ),
        );
    }
    if let Some(s) = &c.summary {
        r = r.field("result", clip(s, 200));
    }
    r = r
        .heading("input")
        .code(serde_json::to_string_pretty(&c.input).unwrap_or_default());
    match inv {
        None => r = r.note("(no stored output: denied, reused, or not run)"),
        Some(inv) => {
            if let Some(cmd) = &inv.command {
                r = r.field("command", cmd.join(" "));
            }
            let body = serde_json::to_string_pretty(&inv.output).unwrap_or_default();
            let lines: Vec<&str> = body.lines().collect();
            r = r
                .heading(format!("output (invocation {})", inv.id.short()))
                .code(
                    lines
                        .iter()
                        .take(40)
                        .map(|l| clip(l, 160))
                        .collect::<Vec<_>>()
                        .join("\n"),
                );
            if lines.len() > 40 {
                r = r.note(format!("… {} more lines", lines.len() - 40));
            }
            if let Some(e) = &inv.error {
                r = r.error(format!("error: {e}"));
            }
        }
    }
    r.data("call", c)
}

pub struct Status;
#[async_trait::async_trait]
impl Command for Status {
    meta!("status", "Agent, model, session and context at a glance");
    async fn run(&self, ix: &mut Interaction, _args: &str) -> anyhow::Result<Reply> {
        let context = match ix.last_context() {
            Some(r) => format!(
                "{} / {} (last request)",
                tokens(r.total as u64),
                tokens(r.budget as u64)
            ),
            None => {
                let p = ix.ark().context_preview(ix.session()).await?;
                format!(
                    "{} / {} (next request)",
                    tokens(p.total as u64),
                    tokens(p.budget as u64)
                )
            }
        };
        let grants = ix.gate().grants();
        let s = ix.session();
        let mut r = Report::titled(ix.app_name().to_string())
            .field("model", ix.ark().router().describe())
            .field("project", ix.project().name.clone())
            .field(
                "session",
                format!("{} · {} turns", s.id().short(), s.turn_count()),
            )
            .field("context", context)
            .field(
                "allowed",
                if grants.is_empty() {
                    "nothing pre-approved".into()
                } else {
                    grants.join(", ")
                },
            )
            .field(
                "detail",
                if ix.detail() {
                    "debug view on"
                } else {
                    "normal (/debug for details)"
                },
            );
        for (title, blocks) in ix.status_sections() {
            r = r.heading(title).blocks(blocks);
        }
        Ok(r.data("summary", ix.summary()).into())
    }
}

pub struct Config;
#[async_trait::async_trait]
impl Command for Config {
    meta!("config", "Effective runtime configuration");
    async fn run(&self, ix: &mut Interaction, _args: &str) -> anyhow::Result<Reply> {
        let s = ix.ark().settings();
        let p = ix.session().record().policy.clone();
        let on = |b: bool| if b { "on" } else { "off" };
        Ok(Report::titled("Config")
            .field("store", s.store_root.display().to_string())
            .field(
                "context budget",
                match s.context_budget {
                    Some(b) => format!("{b} tokens per request (ARK_CTX_BUDGET)"),
                    None => {
                        "by model: a quarter of its window, 6000 to 48000 tokens per request".into()
                    }
                },
            )
            .field("max steps", s.max_steps.to_string())
            .field(
                "planner / critic",
                format!("{} / {}", on(s.planner), on(s.critic)),
            )
            .field(
                "changes",
                if s.allow_mutate {
                    "run without asking"
                } else {
                    "ask first"
                },
            )
            .field(
                "carry-over",
                format!(
                    "last {} turns (clipped to {}/{} chars), {} gists",
                    p.recent_turns, p.input_chars, p.answer_chars, p.max_gists
                ),
            )
            .data("config", &s)
            .into())
    }
}

pub struct Processes;
#[async_trait::async_trait]
impl Command for Processes {
    meta!(
        "processes",
        "What agents started and still runs; stop one with /processes stop <id>"
    );
    fn usage(&self) -> &str {
        "[stop <id> | <id>]"
    }
    async fn run(&self, ix: &mut Interaction, args: &str) -> anyhow::Result<Reply> {
        use ark_sdk::prelude::ark_proc::State;
        let Some(procs) = ix.ark().processes().cloned() else {
            return Ok(Report::new()
                .note("This app does not let agents start processes.")
                .into());
        };
        let (verb, rest) = args.trim().split_once(' ').unwrap_or((args.trim(), ""));
        let mut r = Report::new();
        match (verb, rest.trim()) {
            ("" | "list", _) => {}
            ("stop", id) if !id.is_empty() => {
                r = match procs.stop(id).await {
                    Some(p) => r.success(format!("Stopped {} `{}`.", p.id, p.command)),
                    None => r.warning(format!("No process `{id}`.")),
                };
            }
            (id, "") if procs.get(id).is_some() => {
                let p = procs.get(id).unwrap_or_else(|| unreachable!());
                let out = procs.output(id, 4_000).unwrap_or_default();
                return Ok(r
                    .heading(format!("{} {}", p.id, p.command))
                    .text(if out.trim().is_empty() {
                        "(no output yet)".into()
                    } else {
                        out
                    })
                    .into());
            }
            _ => anyhow::bail!("usage: /processes {}", self.usage()),
        }
        let all = procs.list();
        if all.is_empty() {
            return Ok(r
                .note("Nothing runs. Agents ask before they start a process.")
                .into());
        }
        let mut t = Table::plain(4);
        for p in &all {
            let (state, tone) = match p.state {
                State::Ready => (
                    format!("ready {}", p.url.clone().unwrap_or_default()),
                    Tone::Success,
                ),
                State::Running => ("running".to_string(), Tone::Normal),
                State::Exited => (format!("ended, exit {}", p.code.unwrap_or(-1)), Tone::Muted),
                State::Stopped => ("stopped".to_string(), Tone::Muted),
            };
            let running = matches!(p.state, State::Running | State::Ready);
            let place = match p.folder.as_str() {
                "" => p.project.clone(),
                f => format!("{}/{f}", p.project),
            };
            t.push(Row {
                cells: vec![
                    Cell::new(p.id.clone(), Tone::Accent),
                    Cell::new(clip(&p.command, 60), Tone::Normal),
                    Cell::new(state, tone),
                    Cell::new(place, Tone::Muted),
                ],
                tone: Tone::Normal,
                command: running.then(|| format!("/processes stop {}", p.id)),
            });
        }
        Ok(r.table(t).into())
    }
}
