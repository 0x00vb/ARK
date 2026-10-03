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
            return Ok(crate::orchestration::graph_report(&g).into());
        }
        let events = o.task_trace(task)?;
        Ok(crate::orchestration::task_report(&g, task, &events).into())
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

pub struct TaskCmd;
#[async_trait::async_trait]
impl Command for TaskCmd {
    meta!(
        "task",
        "Hand an agent a task, or cancel or retry one: /task <agent> <objective>"
    );
    fn usage(&self) -> &str {
        "<agent> <objective> | cancel|retry <task>"
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
        let mut actions = ix.engine().tools().actions();
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
                format!("{} tokens per request (ARK_CTX_BUDGET)", s.context_budget),
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
