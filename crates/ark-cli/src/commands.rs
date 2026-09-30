//! Slash commands. ARK owns the infrastructure (parsing, dispatch, completion, help) and a set
//! of generic commands that map onto real runtime capabilities. A consumer adds its own domain
//! commands by implementing [`Command`]; they appear in `/help` and tab completion next to the
//! built-ins, and they get the same [`CommandContext`].

use std::sync::Arc;

use ark_sdk::prelude::*;

use crate::app::{ShellState, TurnRecord};
use crate::term::{clip, tokens};
use crate::theme::Theme;

/// What a command wants the shell to do next.
pub enum CommandOutcome {
    /// Back to the prompt.
    Continue,
    /// Send this text to the agent as if the user typed it.
    Submit(String),
    /// Leave the shell.
    Exit,
}

/// A slash command. Implement this for domain commands (`/device`, `/chapter`, …).
#[async_trait::async_trait]
pub trait Command: Send + Sync {
    /// The name without the slash.
    fn name(&self) -> &str;
    /// One line for `/help` and completion.
    fn summary(&self) -> &str;
    /// Argument hint, e.g. `<session>`.
    fn usage(&self) -> &str {
        ""
    }
    fn aliases(&self) -> &[&str] {
        &[]
    }
    /// The `/help` section it is listed under.
    fn group(&self) -> &str {
        "Commands"
    }
    async fn run(&self, ctx: &mut CommandContext<'_>, args: &str)
        -> anyhow::Result<CommandOutcome>;
}

/// What a command can see and do.
pub struct CommandContext<'a> {
    pub(crate) st: &'a mut ShellState,
}

impl<'a> CommandContext<'a> {
    pub fn ark(&self) -> &Ark {
        &self.st.ark
    }
    pub fn engine(&self) -> &Engine {
        self.st.ark.engine()
    }
    pub fn project(&self) -> &Project {
        &self.st.project
    }
    pub fn session(&self) -> &Session {
        &self.st.session
    }
    pub fn theme(&self) -> &Theme {
        &self.st.cfg.theme
    }
    pub fn app_name(&self) -> &str {
        &self.st.cfg.name
    }
    pub fn verbose(&self) -> bool {
        self.st.verbose
    }
    /// The last request's context breakdown, if a turn has run.
    pub fn last_context(&self) -> Option<&ContextReport> {
        self.st.last_context.as_ref()
    }
    pub fn last_turn(&self) -> Option<&TurnRecord> {
        self.st.last_turn.as_ref()
    }

    /// Print one line.
    pub fn line(&mut self, s: impl AsRef<str>) {
        self.st.out.line(s.as_ref());
    }
    pub fn blank(&mut self) {
        self.st.out.line("");
    }
    /// A section title.
    pub fn heading(&mut self, s: &str) {
        let t = self.st.cfg.theme.bold(&self.st.cfg.theme.accented(s));
        self.st.out.line(&t);
    }
    /// An aligned `label  value` row.
    pub fn row(&mut self, label: &str, value: impl AsRef<str>) {
        let l = self.st.cfg.theme.dim(&format!("{label:<22} "));
        self.st.out.line(&format!("  {l}{}", value.as_ref()));
    }

    /// Run an action directly, recorded as an invocation like any agent call. The human typed
    /// the command, so a state-changing action runs only when `approved` is true.
    pub async fn run_action(
        &self,
        action: &str,
        input: serde_json::Value,
        approved: bool,
    ) -> anyhow::Result<ark_core::Invocation> {
        Ok(self
            .engine()
            .run_action(&self.st.project.id, action, input, approved)
            .await?)
    }

    /// Start a fresh session in the current project. Session approval grants are cleared.
    pub fn new_session(&mut self) -> anyhow::Result<()> {
        self.st.session = self.st.ark.start_session(&self.st.project.id)?;
        self.st.gate.revoke_all();
        self.st.last_turn = None;
        self.st.last_context = None;
        Ok(())
    }

    /// Work in another project from now on, in a fresh session there.
    pub fn switch_project(&mut self, project: Project) -> anyhow::Result<()> {
        self.st.project = project;
        self.new_session()
    }

    /// Switch to another session by id or prefix. Returns false when none matches.
    pub fn resume(&mut self, id: &str) -> anyhow::Result<bool> {
        let Some(s) = self.st.ark.open_session(id)? else {
            return Ok(false);
        };
        if s.project() != &self.st.project.id {
            if let Some(p) = self.engine().store.get_project(s.project())? {
                self.st.project = p;
            }
        }
        self.st.session = s;
        self.st.gate.revoke_all();
        self.st.last_turn = None;
        self.st.last_context = None;
        Ok(true)
    }
}

/// Split `/name rest of line` into `("name", "rest of line")`.
pub(crate) fn parse(line: &str) -> Option<(&str, &str)> {
    let body = line.strip_prefix('/')?;
    if body.is_empty() || body.starts_with(' ') {
        return None;
    }
    Some(match body.split_once(char::is_whitespace) {
        Some((n, rest)) => (n, rest.trim()),
        None => (body, ""),
    })
}

/// The generic commands every ARK shell has.
pub(crate) fn builtins() -> Vec<Arc<dyn Command>> {
    vec![
        Arc::new(Help),
        Arc::new(New),
        Arc::new(Resume),
        Arc::new(Sessions),
        Arc::new(Model),
        Arc::new(Context),
        Arc::new(Usage),
        Arc::new(Tools),
        Arc::new(Trace),
        Arc::new(State),
        Arc::new(Status),
        Arc::new(Compact),
        Arc::new(Clear),
        Arc::new(Config),
        Arc::new(Debug),
        Arc::new(Exit),
    ]
}

macro_rules! meta {
    ($name:literal, $summary:literal) => {
        fn name(&self) -> &str {
            $name
        }
        fn summary(&self) -> &str {
            $summary
        }
        fn group(&self) -> &str {
            "Session"
        }
    };
}

struct Help;
#[async_trait::async_trait]
impl Command for Help {
    meta!("help", "Show commands and keys");
    fn aliases(&self) -> &[&str] {
        &["?"]
    }
    async fn run(
        &self,
        ctx: &mut CommandContext<'_>,
        _args: &str,
    ) -> anyhow::Result<CommandOutcome> {
        let cfg = ctx.st.cfg.clone();
        if let Some(intro) = &cfg.help_intro {
            for l in intro.lines() {
                ctx.line(format!("  {l}"));
            }
            ctx.blank();
        }
        let mut groups: Vec<String> = Vec::new();
        for c in &cfg.commands {
            if !groups.iter().any(|g| g == c.group()) {
                groups.push(c.group().to_string());
            }
        }
        // Domain groups first, the generic session commands after.
        groups.sort_by_key(|g| g == "Session");
        for g in groups {
            ctx.heading(&g);
            for c in cfg.commands.iter().filter(|c| c.group() == g) {
                let name = format!("/{} {}", c.name(), c.usage());
                let t = ctx.theme().bold(&format!("{:<27} ", name.trim_end()));
                ctx.line(format!("  {t}{}", ctx.theme().dim(c.summary())));
            }
            ctx.blank();
        }
        ctx.heading("Keys");
        for (k, v) in [
            ("enter", "send"),
            ("alt+enter, ctrl+j", "new line (or end a line with \\)"),
            ("↑ / ↓", "input history"),
            ("tab", "complete a /command"),
            ("ctrl+c, esc", "stop the agent (while it works)"),
            ("ctrl+d, /exit", "leave (the session is saved)"),
        ] {
            ctx.line(format!(
                "  {}{}",
                ctx.theme().bold(&format!("{k:<27} ")),
                ctx.theme().dim(v)
            ));
        }
        Ok(CommandOutcome::Continue)
    }
}

struct New;
#[async_trait::async_trait]
impl Command for New {
    meta!(
        "new",
        "Start a new session (working state and evidence are kept)"
    );
    async fn run(
        &self,
        ctx: &mut CommandContext<'_>,
        _args: &str,
    ) -> anyhow::Result<CommandOutcome> {
        ctx.new_session()?;
        let id = ctx.session().id().short().to_string();
        ctx.line(format!(
            "{} new session {}",
            ctx.theme().green(&ctx.theme().glyphs.ok),
            ctx.theme().dim(&id)
        ));
        Ok(CommandOutcome::Continue)
    }
}

struct Resume;
#[async_trait::async_trait]
impl Command for Resume {
    meta!("resume", "Continue an earlier session");
    fn usage(&self) -> &str {
        "[session]"
    }
    async fn run(
        &self,
        ctx: &mut CommandContext<'_>,
        args: &str,
    ) -> anyhow::Result<CommandOutcome> {
        let target = if args.is_empty() {
            // The most recent session other than this one.
            let current = ctx.session().id().clone();
            Session::list(ctx.engine(), Some(&ctx.project().id))?
                .into_iter()
                .find(|r| r.id != current && r.turn_count > 0)
                .map(|r| r.id.as_str().to_string())
        } else {
            Some(args.to_string())
        };
        let Some(target) = target else {
            ctx.line(
                ctx.theme()
                    .dim("No other session to resume. /sessions lists them."),
            );
            return Ok(CommandOutcome::Continue);
        };
        if !ctx.resume(&target)? {
            ctx.line(ctx.theme().yellow(&format!(
                "No session matches `{target}`. /sessions lists them."
            )));
            return Ok(CommandOutcome::Continue);
        }
        crate::shell::print_recap(ctx.st);
        Ok(CommandOutcome::Continue)
    }
}

struct Sessions;
#[async_trait::async_trait]
impl Command for Sessions {
    meta!("sessions", "List sessions in this project");
    async fn run(
        &self,
        ctx: &mut CommandContext<'_>,
        _args: &str,
    ) -> anyhow::Result<CommandOutcome> {
        let current = ctx.session().id().clone();
        let list = Session::list(ctx.engine(), Some(&ctx.project().id))?;
        if list.is_empty() {
            ctx.line(ctx.theme().dim("No sessions yet."));
        }
        for r in list.iter().take(20) {
            let mark = if r.id == current {
                ctx.theme().accented("›")
            } else {
                " ".into()
            };
            let title = if r.title.is_empty() {
                ctx.theme().dim("(empty)")
            } else {
                clip(&r.title, 50)
            };
            let meta = ctx
                .theme()
                .dim(&format!("{} turns · {}", r.turn_count, ago(r.updated_at)));
            ctx.line(format!(
                "  {mark} {}  {title}  {meta}",
                ctx.theme().bold(r.id.short())
            ));
        }
        if list.len() > 20 {
            ctx.line(ctx.theme().dim(&format!("  … {} more", list.len() - 20)));
        }
        ctx.line(ctx.theme().dim("  /resume <id> to switch"));
        Ok(CommandOutcome::Continue)
    }
}

struct Model;
#[async_trait::async_trait]
impl Command for Model {
    meta!(
        "model",
        "Show the model, or switch: /model <model> or /model <provider> <model>"
    );
    fn usage(&self) -> &str {
        "[provider] [model]"
    }
    async fn run(
        &self,
        ctx: &mut CommandContext<'_>,
        args: &str,
    ) -> anyhow::Result<CommandOutcome> {
        let router = ctx.ark().router();
        let (reason, plan, routing) = (
            router.provider(Role::Reason).describe(),
            router.provider(Role::Plan).describe(),
            router.describe(),
        );
        if args.is_empty() {
            ctx.row("reasoning", reason);
            ctx.row("planning", plan);
            ctx.row("routing", routing);
            return Ok(CommandOutcome::Continue);
        }
        let parts: Vec<&str> = args.split_whitespace().collect();
        let (provider, model) = match parts.as_slice() {
            [model] => (
                reason
                    .split_whitespace()
                    .next()
                    .unwrap_or("ollama")
                    .to_string(),
                model.to_string(),
            ),
            [provider, model, ..] => (provider.to_string(), model.to_string()),
            [] => unreachable!(),
        };
        match ark_model::build(&provider, Some(model)) {
            Ok(p) => {
                let d = p.describe();
                ctx.st.ark.set_router(ModelRouter::single(p));
                ctx.line(format!(
                    "{} now using {}",
                    ctx.theme().green(&ctx.theme().glyphs.ok),
                    ctx.theme().bold(&d)
                ));
            }
            Err(e) => ctx.line(ctx.theme().red(&format!("Could not switch model: {e}"))),
        }
        Ok(CommandOutcome::Continue)
    }
}

struct Context;
#[async_trait::async_trait]
impl Command for Context {
    meta!("context", "What the model receives, by source");
    async fn run(
        &self,
        ctx: &mut CommandContext<'_>,
        _args: &str,
    ) -> anyhow::Result<CommandOutcome> {
        let preview = ctx.ark().context_preview(ctx.session()).await?;
        let theme = ctx.theme().clone();
        let (report, label) = match ctx.last_context() {
            Some(r) => (r.clone(), format!("last request (step {})", r.step)),
            None => (preview.clone(), "next request (preview)".to_string()),
        };
        ctx.heading(&format!("Context · {label}"));
        let width = 34usize;
        let rule = theme.dim(&"─".repeat(width));
        ctx.line(format!("  {rule}"));
        let cell = |name: &str, v: String| format!("  {name:<22}{v:>12}");
        for (name, n) in report.rows() {
            let text = cell(name, tokens(n as u64));
            ctx.line(if n == 0 { theme.dim(&text) } else { text });
        }
        ctx.line(format!("  {rule}"));
        ctx.line(theme.bold(&cell("Used", tokens(report.total as u64))));
        ctx.line(cell("Budget", tokens(report.budget as u64)));
        ctx.line(theme.dim(&cell(
            "Tool schemas (extra)",
            tokens(report.tool_schemas as u64),
        )));
        if report.compressed_rounds > 0 {
            ctx.line(theme.dim(&format!(
                "  {} older round(s) compressed out of the window",
                report.compressed_rounds
            )));
        }
        if ctx.last_context().is_some() {
            ctx.line(theme.dim(&format!(
                "  next turn starts at about {} (preview)",
                tokens(preview.total as u64)
            )));
        }
        ctx.blank();

        // What ARK kept out of the prompt, and how.
        let rec = ctx.session().record().clone();
        let transcript = ctx.session().transcript(ctx.engine(), None)?;
        let transcript_chars: usize = transcript
            .iter()
            .map(|t| t.input.len() + t.answer.len())
            .sum();
        let carry = ctx
            .session()
            .conversation_digest()
            .map(|d| ark_model::estimate_tokens(&d))
            .unwrap_or(0);
        let pid = ctx.project().id.clone();
        let invs = ctx.engine().store.list_invocations(&pid)?;
        let cards = ctx.engine().store.list_evidence_cards(&pid)?;
        let raw: usize = invs.iter().map(|i| i.output.to_string().len()).sum();
        let distilled: usize = cards
            .iter()
            .map(|c| c.summary.len() + c.identifiers.iter().map(|x| x.len()).sum::<usize>())
            .sum();
        ctx.heading("Kept out of the prompt");
        ctx.row(
            "transcript",
            format!(
                "{} turns stored (~{} tokens of text), {} tokens carried into the next turn",
                rec.turn_count,
                tokens((transcript_chars / 4) as u64),
                tokens(carry as u64)
            ),
        );
        ctx.row(
            "folded turns",
            format!(
                "{} as one-line gists, {} merged into working state",
                rec.gists.len(),
                rec.dropped_turns
            ),
        );
        ctx.row(
            "tool output",
            format!(
                "{} calls, ~{} raw → ~{} distilled into {} evidence cards",
                invs.len(),
                tokens((raw / 4) as u64),
                tokens((distilled / 4) as u64),
                cards.len()
            ),
        );
        if rec.compactions > 0 {
            ctx.row("compactions", rec.compactions.to_string());
        }
        Ok(CommandOutcome::Continue)
    }
}

struct Usage;
#[async_trait::async_trait]
impl Command for Usage {
    meta!("usage", "Tokens, model calls and tools used this session");
    async fn run(
        &self,
        ctx: &mut CommandContext<'_>,
        _args: &str,
    ) -> anyhow::Result<CommandOutcome> {
        let u = ctx.session().usage().clone();
        let approx = if u.measured || u.turns == 0 { "" } else { "~" };
        ctx.heading("Usage · this session");
        ctx.row("turns", u.turns.to_string());
        ctx.row(
            "tokens",
            format!(
                "{approx}{} ({approx}{} in / {approx}{} out)",
                tokens(u.total_tokens()),
                tokens(u.prompt_tokens),
                tokens(u.completion_tokens)
            ),
        );
        ctx.row(
            "largest request",
            format!("{} tokens", tokens(u.peak_prompt_tokens as u64)),
        );
        ctx.row("model calls", u.model_calls.to_string());
        ctx.row(
            "tool calls",
            format!(
                "{} ({} repeats avoided)",
                u.tool_calls, u.duplicate_tool_calls
            ),
        );
        ctx.row("time", format!("{:.1}s", u.elapsed_ms as f64 / 1000.0));
        if !u.measured && u.turns > 0 {
            ctx.line(
                ctx.theme()
                    .dim("  ~ some numbers are estimates (the provider did not report usage)"),
            );
        }
        Ok(CommandOutcome::Continue)
    }
}

struct Tools;
#[async_trait::async_trait]
impl Command for Tools {
    meta!("tools", "The tools this agent can use");
    async fn run(
        &self,
        ctx: &mut CommandContext<'_>,
        _args: &str,
    ) -> anyhow::Result<CommandOutcome> {
        let mut actions = ctx.engine().registry.actions();
        actions.sort_by(|a, b| a.name.cmp(&b.name));
        let core = ctx.ark().profile().core_actions.clone();
        let theme = ctx.theme().clone();
        let width = ctx.st.out.width().saturating_sub(48).max(20);
        for d in &actions {
            let risk = match d.risk {
                ark_core::RiskTier::Read => theme.green("read   "),
                ark_core::RiskTier::Observe => theme.code("observe"),
                ark_core::RiskTier::Mutate => theme.yellow("changes"),
            };
            let pin = if core.contains(&d.name) {
                theme.dim(" ·preloaded")
            } else {
                String::new()
            };
            ctx.line(format!(
                "  {risk}  {}{pin}  {}",
                theme.bold(&format!("{:<24}", d.name)),
                theme.dim(&clip(&d.summary, width))
            ));
        }
        ctx.line(theme.dim(&format!(
            "  {} actions. `changes` asks before running. Schemas load on demand; the model sees a one-line index.",
            actions.len()
        )));
        Ok(CommandOutcome::Continue)
    }
}

struct Trace;
#[async_trait::async_trait]
impl Command for Trace {
    meta!(
        "trace",
        "Inspect the last turn: every step, call, and cost. /trace <n> shows call n"
    );
    fn usage(&self) -> &str {
        "[n]"
    }
    async fn run(
        &self,
        ctx: &mut CommandContext<'_>,
        args: &str,
    ) -> anyhow::Result<CommandOutcome> {
        let Some(turn) = ctx.st.last_turn.take() else {
            ctx.line(ctx.theme().dim("Nothing to trace yet."));
            return Ok(CommandOutcome::Continue);
        };
        let r = if args.is_empty() {
            trace_timeline(ctx, &turn)
        } else {
            trace_call(ctx, &turn, args).await
        };
        ctx.st.last_turn = Some(turn);
        r?;
        Ok(CommandOutcome::Continue)
    }
}

fn trace_timeline(ctx: &mut CommandContext<'_>, turn: &TurnRecord) -> anyhow::Result<()> {
    let theme = ctx.theme().clone();
    ctx.heading(&format!("Trace · {}", clip(&turn.input, 60)));
    let mut call_n = 0;
    for (ms, ev) in &turn.events {
        let t = theme.dim(&format!("{:>7.2}s", *ms as f64 / 1000.0));
        let line = match ev {
            HarnessEvent::Step { index } => theme.bold(&format!("step {index}")),
            HarnessEvent::Plan { steps } => format!("plan: {}", steps.join(" → ")),
            HarnessEvent::Context { report } => theme.dim(&format!(
                "context {} / {} (state {}, conversation {}, tool results {}, compressed {})",
                tokens(report.total as u64),
                tokens(report.budget as u64),
                report.working_state,
                report.conversation,
                report.tool_rounds,
                report.compressed_rounds
            )),
            HarnessEvent::ModelCall {
                role,
                model,
                usage,
                elapsed_ms,
            } => theme.dim(&format!(
                "model {role} · {model} · {}→{} tokens{} · {:.1}s",
                usage.prompt_tokens,
                usage.completion_tokens,
                if usage.measured { "" } else { " (est.)" },
                *elapsed_ms as f64 / 1000.0
            )),
            HarnessEvent::TextDelta { text } => {
                theme.dim(&format!("text {} chars: {}", text.len(), clip(text, 60)))
            }
            HarnessEvent::ToolCall { action, input, .. } => {
                call_n += 1;
                format!(
                    "{} {} {}",
                    theme.accented(&format!("#{call_n}")),
                    theme.bold(action),
                    theme.dim(&clip(&input.to_string(), 80))
                )
            }
            HarnessEvent::ToolResult {
                ok,
                summary,
                duration_ms,
                invocation,
                error,
                ..
            } => {
                let mark = if *ok {
                    theme.green(&theme.glyphs.ok)
                } else {
                    theme.red(&theme.glyphs.fail)
                };
                let d = duration_ms.map(|d| format!(" {d}ms")).unwrap_or_default();
                let e = error
                    .as_ref()
                    .map(|e| format!(" error: {e}"))
                    .unwrap_or_default();
                format!(
                    "  {mark} {}{}{} {}",
                    clip(summary, 70),
                    theme.dim(&d),
                    theme.red(&e),
                    theme.dim(&format!("inv:{}", &invocation[..8.min(invocation.len())]))
                )
            }
            HarnessEvent::Duplicate { action, reused } => {
                format!("  reused {action}: {}", clip(reused, 60))
            }
            HarnessEvent::Retrieval { what } => format!("  recall {what}"),
            HarnessEvent::ApprovalRequested { request } => format!("  asked: {}", request.preview),
            HarnessEvent::ApprovalResolved {
                decision, class, ..
            } => format!("  approval {decision:?} ({class})"),
            HarnessEvent::NeedsApproval { action } => format!("  refused {action} (no approver)"),
            HarnessEvent::StateUpdate { kind, text } => {
                format!("  state {kind}: {}", clip(text, 60))
            }
            HarnessEvent::Note { kind, body } => format!("  note {kind}: {}", clip(body, 60)),
            HarnessEvent::Capability { action, detail } => {
                format!("  {action}: {}", clip(detail, 60))
            }
            HarnessEvent::Present { directive } => format!("  present {}", directive.headline()),
            HarnessEvent::SelfCheck { passed, detail } => format!(
                "  self-check {}: {detail}",
                if *passed { "passed" } else { "failed" }
            ),
            HarnessEvent::Classified { trivial } => theme.dim(if *trivial {
                "classified: direct"
            } else {
                "classified: investigation"
            }),
            HarnessEvent::Cancelled => theme.yellow("cancelled"),
            HarnessEvent::Answer { text, .. } => format!("answer ({} chars)", text.len()),
        };
        ctx.line(format!("{t}  {line}"));
    }
    if let Some(e) = &turn.error {
        ctx.line(theme.red(&format!("error: {e}")));
    }
    ctx.line(theme.dim(&format!(
        "{:.2}s total · /trace <n> for a call's input and output",
        turn.elapsed_ms as f64 / 1000.0
    )));
    Ok(())
}

async fn trace_call(
    ctx: &mut CommandContext<'_>,
    turn: &TurnRecord,
    args: &str,
) -> anyhow::Result<()> {
    let n: usize = args
        .trim_start_matches('#')
        .parse()
        .map_err(|_| anyhow::anyhow!("usage: /trace <n>"))?;
    let calls: Vec<&HarnessEvent> = turn
        .events
        .iter()
        .map(|(_, e)| e)
        .filter(|e| matches!(e, HarnessEvent::ToolCall { .. }))
        .collect();
    let Some(HarnessEvent::ToolCall { id, action, input }) = calls.get(n.wrapping_sub(1)).copied()
    else {
        anyhow::bail!("no call #{n} in the last turn ({} calls)", calls.len());
    };
    ctx.heading(&format!("#{n} {action}"));
    ctx.line(ctx.theme().dim("input"));
    for l in serde_json::to_string_pretty(input)?.lines() {
        ctx.line(format!("  {l}"));
    }
    let inv = turn.events.iter().find_map(|(_, e)| match e {
        HarnessEvent::ToolResult {
            id: rid,
            invocation,
            ..
        } if rid == id => Some(invocation.clone()),
        _ => None,
    });
    match inv {
        None => ctx.line(ctx.theme().dim("(not run: denied, reused, or cancelled)")),
        Some(inv_id) => {
            let pid = ctx.project().id.clone();
            if let Some(inv) = ctx.engine().store.get_invocation(&pid, &inv_id)? {
                if let Some(cmd) = &inv.command {
                    ctx.line(ctx.theme().dim(&format!("command: {}", cmd.join(" "))));
                }
                ctx.line(
                    ctx.theme()
                        .dim(&format!("output (invocation {})", inv.id.short())),
                );
                let body = serde_json::to_string_pretty(&inv.output)?;
                let lines: Vec<&str> = body.lines().collect();
                for l in lines.iter().take(40) {
                    ctx.line(format!("  {}", clip(l, 160)));
                }
                if lines.len() > 40 {
                    ctx.line(
                        ctx.theme()
                            .dim(&format!("  … {} more lines", lines.len() - 40)),
                    );
                }
                if let Some(e) = &inv.error {
                    ctx.line(ctx.theme().red(&format!("error: {e}")));
                }
            }
        }
    }
    Ok(())
}

struct State;
#[async_trait::async_trait]
impl Command for State {
    meta!("state", "The working state the agent carries between turns");
    async fn run(
        &self,
        ctx: &mut CommandContext<'_>,
        _args: &str,
    ) -> anyhow::Result<CommandOutcome> {
        let pid = ctx.project().id.clone();
        let state = ctx
            .engine()
            .store
            .get_research_state(&pid)?
            .unwrap_or_default();
        if state.is_empty() && state.goal.is_empty() {
            ctx.line(ctx.theme().dim("The working state is empty."));
            return Ok(CommandOutcome::Continue);
        }
        let digest = state.digest(8);
        let n = ark_model::estimate_tokens(&digest);
        ctx.heading(&format!("Working state · {} tokens", tokens(n as u64)));
        for l in digest.lines() {
            ctx.line(format!("  {l}"));
        }
        Ok(CommandOutcome::Continue)
    }
}

struct Status;
#[async_trait::async_trait]
impl Command for Status {
    meta!("status", "Agent, model, session and context at a glance");
    async fn run(
        &self,
        ctx: &mut CommandContext<'_>,
        _args: &str,
    ) -> anyhow::Result<CommandOutcome> {
        let cfg = ctx.st.cfg.clone();
        ctx.heading(&cfg.name);
        ctx.row("model", ctx.ark().router().describe());
        ctx.row("project", ctx.project().name.clone());
        let s = ctx.session();
        let sid = format!("{} · {} turns", s.id().short(), s.turn_count());
        ctx.row("session", sid);
        let ctx_line = match ctx.last_context() {
            Some(r) => format!(
                "{} / {} (last request)",
                tokens(r.total as u64),
                tokens(r.budget as u64)
            ),
            None => {
                let p = ctx.ark().context_preview(ctx.session()).await?;
                format!(
                    "{} / {} (next request)",
                    tokens(p.total as u64),
                    tokens(p.budget as u64)
                )
            }
        };
        ctx.row("context", ctx_line);
        let grants = ctx.st.gate.grants();
        ctx.row(
            "allowed",
            if grants.is_empty() {
                "nothing pre-approved".into()
            } else {
                grants.join(", ")
            },
        );
        ctx.row(
            "detail",
            if ctx.verbose() {
                "debug view on"
            } else {
                "normal (/debug for details)"
            },
        );
        for (title, f) in &cfg.status {
            if let Some(body) = f(ctx.ark(), ctx.project()) {
                ctx.blank();
                ctx.heading(title);
                for l in body.lines() {
                    ctx.line(format!("  {l}"));
                }
            }
        }
        Ok(CommandOutcome::Continue)
    }
}

struct Compact;
#[async_trait::async_trait]
impl Command for Compact {
    meta!(
        "compact",
        "Tighten the carried context (keeps goals, findings, open questions)"
    );
    async fn run(
        &self,
        ctx: &mut CommandContext<'_>,
        _args: &str,
    ) -> anyhow::Result<CommandOutcome> {
        let st = &mut *ctx.st;
        let r = st.session.compact(st.ark.engine())?;
        st.last_context = None;
        let theme = st.cfg.theme.clone();
        ctx.line(format!("{} compacted", theme.green(&theme.glyphs.ok)));
        ctx.row(
            "conversation",
            format!(
                "{} → {} tokens ({} turns folded, {} gists merged)",
                r.conversation_before, r.conversation_after, r.turns_folded, r.gists_dropped
            ),
        );
        ctx.row(
            "working state",
            format!(
                "{} → {} tokens ({} stale or duplicate items removed)",
                r.state_before, r.state_after, r.state_items_removed
            ),
        );
        ctx.line(
            theme.dim("  Nothing was deleted: the transcript and all evidence remain retrievable."),
        );
        Ok(CommandOutcome::Continue)
    }
}

struct Clear;
#[async_trait::async_trait]
impl Command for Clear {
    meta!("clear", "Clear the screen (the conversation is kept)");
    async fn run(
        &self,
        ctx: &mut CommandContext<'_>,
        _args: &str,
    ) -> anyhow::Result<CommandOutcome> {
        ctx.st.out.text("\x1b[2J\x1b[3J\x1b[H");
        Ok(CommandOutcome::Continue)
    }
}

struct Config;
#[async_trait::async_trait]
impl Command for Config {
    meta!("config", "Effective runtime configuration");
    async fn run(
        &self,
        ctx: &mut CommandContext<'_>,
        _args: &str,
    ) -> anyhow::Result<CommandOutcome> {
        let s = ctx.ark().settings();
        let p = ctx.session().record().policy.clone();
        ctx.heading("Config");
        ctx.row("store", s.store_root.display().to_string());
        ctx.row(
            "context budget",
            format!("{} tokens per request (ARK_CTX_BUDGET)", s.context_budget),
        );
        ctx.row("max steps", s.max_steps.to_string());
        ctx.row(
            "planner / critic",
            format!("{} / {}", on(s.planner), on(s.critic)),
        );
        ctx.row(
            "changes",
            if s.allow_mutate {
                "run without asking".into()
            } else {
                "ask first".to_string()
            },
        );
        ctx.row(
            "carry-over",
            format!(
                "last {} turns (clipped to {}/{} chars), {} gists",
                p.recent_turns, p.input_chars, p.answer_chars, p.max_gists
            ),
        );
        if let Some(h) = &ctx.st.cfg.history_file {
            ctx.row("input history", h.display().to_string());
        }
        Ok(CommandOutcome::Continue)
    }
}

struct Debug;
#[async_trait::async_trait]
impl Command for Debug {
    meta!(
        "debug",
        "Toggle the detailed view (arguments, durations, model calls)"
    );
    fn usage(&self) -> &str {
        "[on|off]"
    }
    async fn run(
        &self,
        ctx: &mut CommandContext<'_>,
        args: &str,
    ) -> anyhow::Result<CommandOutcome> {
        ctx.st.verbose = match args {
            "on" => true,
            "off" => false,
            _ => !ctx.st.verbose,
        };
        let msg = if ctx.st.verbose {
            "debug view on"
        } else {
            "debug view off"
        };
        ctx.line(ctx.theme().dim(msg));
        Ok(CommandOutcome::Continue)
    }
}

struct Exit;
#[async_trait::async_trait]
impl Command for Exit {
    meta!("exit", "Leave (the session is saved; resume with --resume)");
    fn aliases(&self) -> &[&str] {
        &["quit"]
    }
    async fn run(
        &self,
        _ctx: &mut CommandContext<'_>,
        _args: &str,
    ) -> anyhow::Result<CommandOutcome> {
        Ok(CommandOutcome::Exit)
    }
}

fn on(b: bool) -> &'static str {
    if b {
        "on"
    } else {
        "off"
    }
}

/// `3m ago`, `2h ago`, `4d ago`.
pub(crate) fn ago(t: time::OffsetDateTime) -> String {
    let secs = (time::OffsetDateTime::now_utc() - t).whole_seconds().max(0);
    match secs {
        0..=59 => "just now".into(),
        60..=3599 => format!("{}m ago", secs / 60),
        3600..=86_399 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_splits_name_and_args() {
        assert_eq!(parse("/resume 8f3a"), Some(("resume", "8f3a")));
        assert_eq!(parse("/help"), Some(("help", "")));
        assert_eq!(
            parse("/model  anthropic  claude"),
            Some(("model", "anthropic  claude"))
        );
        assert_eq!(parse("/ not a command"), None);
        assert_eq!(parse("plain text"), None);
        assert_eq!(parse("/"), None);
    }

    #[test]
    fn builtin_names_are_unique() {
        let b = builtins();
        let mut names: Vec<&str> = b.iter().map(|c| c.name()).collect();
        names.sort();
        let n = names.len();
        names.dedup();
        assert_eq!(n, names.len());
    }
}
