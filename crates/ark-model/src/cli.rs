//! Subscription models through a vendor's official CLI: Claude Code (`claude`, a Claude
//! subscription) and Codex (`codex`, a ChatGPT plan). ARK never reads their credentials; each
//! completion runs the CLI once and the CLI signs the request with its own login.
//!
//! The CLIs are agents with their own tools. Here those tools are off (`claude --tools ""`;
//! Codex runs read-only and is told it has none), and ARK's tools are described in the system
//! prompt instead. The reply is JSON checked against [`REPLY_SCHEMA`]: either text for the
//! person or a list of tool calls. ARK's loop runs the tools, so approvals, traces and budgets
//! work as with any other provider.
//!
//! As a plain [`Provider`], each call starts a process and resends the whole transcript. As a
//! [`SessionBackend`], driven by a [`crate::session::ManagedSession`], a vendor session is
//! opened once (Claude `--session-id`, Codex `exec`) and continued with only what is new
//! (Claude `--resume`, Codex `exec resume`). No streaming: text arrives when the reply is
//! complete.

use crate::session::{Base, Delta, SessionBackend};
use crate::{Completion, Message, Provider, ProviderError, Result, Role, ToolCall, ToolDef, Usage};
use async_trait::async_trait;
use serde::Serialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

/// How long one completion may take before the CLI is stopped.
const CALL_TIMEOUT: Duration = Duration::from_secs(600);

/// The shape every reply must have. Strict enough for OpenAI structured outputs: tool
/// arguments travel as a JSON string because an open object is not allowed there.
pub const REPLY_SCHEMA: &str = r#"{"type":"object","additionalProperties":false,"properties":{"action":{"type":"string","enum":["reply","call_tools"]},"text":{"type":"string"},"tool_calls":{"type":"array","items":{"type":"object","additionalProperties":false,"properties":{"name":{"type":"string"},"arguments":{"type":"string"}},"required":["name","arguments"]}}},"required":["action","text","tool_calls"]}"#;

/// Which vendor CLI.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Cli {
    /// Claude Code, signed in with a Claude subscription.
    Claude,
    /// OpenAI Codex, signed in with ChatGPT.
    Codex,
}

impl Cli {
    /// The provider name used in model specs (`claude:sonnet`, `codex:default`).
    pub fn name(self) -> &'static str {
        match self {
            Cli::Claude => "claude",
            Cli::Codex => "codex",
        }
    }

    fn program(self) -> &'static str {
        self.name()
    }

    fn env_override(self) -> &'static str {
        match self {
            Cli::Claude => "ARK_CLAUDE_BIN",
            Cli::Codex => "ARK_CODEX_BIN",
        }
    }

    /// The model used when a spec names none.
    pub fn default_model(self) -> &'static str {
        match self {
            Cli::Claude => "sonnet",
            Cli::Codex => "default",
        }
    }

    /// Models to offer in a picker. Claude takes aliases; Codex's own default is the safe pick.
    pub fn suggested_models(self) -> &'static [&'static str] {
        match self {
            Cli::Claude => &["sonnet", "opus", "fable"],
            Cli::Codex => &["default"],
        }
    }

    /// The command that signs in, for the person to run.
    pub fn login_command(self) -> &'static str {
        match self {
            Cli::Claude => "claude auth login",
            Cli::Codex => "codex login",
        }
    }

    /// Where to get the CLI when it is missing.
    pub fn install_hint(self) -> &'static str {
        match self {
            Cli::Claude => "npm install -g @anthropic-ai/claude-code",
            Cli::Codex => "npm install -g @openai/codex",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Cli::Claude => "Claude subscription",
            Cli::Codex => "ChatGPT subscription (Codex)",
        }
    }

    /// Find the executable. A GUI app on macOS starts with a minimal `PATH`, so the usual
    /// install places are searched too.
    pub fn locate(self) -> Option<PathBuf> {
        if let Ok(p) = std::env::var(self.env_override()) {
            let p = PathBuf::from(p);
            return p.is_file().then_some(p);
        }
        let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).collect())
            .unwrap_or_default();
        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
            for d in [
                ".local/bin",
                ".claude/local",
                ".npm-global/bin",
                ".bun/bin",
                ".volta/bin",
            ] {
                dirs.push(home.join(d));
            }
            // nvm keeps one bin directory per Node version; newest name last after sorting.
            if let Ok(rd) = std::fs::read_dir(home.join(".nvm/versions/node")) {
                let mut v: Vec<PathBuf> = rd.flatten().map(|e| e.path().join("bin")).collect();
                v.sort();
                dirs.extend(v.into_iter().rev());
            }
        }
        dirs.extend(["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from));
        dirs.into_iter()
            .map(|d| d.join(self.program()))
            .find(|p| p.is_file())
    }
}

/// Whether a CLI is installed and signed in.
#[derive(Clone, Debug, Serialize)]
pub struct CliStatus {
    pub cli: Cli,
    pub installed: bool,
    pub signed_in: bool,
    /// How it is signed in ("claude.ai", "ChatGPT"), or why it is not usable.
    pub detail: String,
}

/// Check one CLI. Never fails: problems come back in `detail`.
pub async fn status(cli: Cli) -> CliStatus {
    let mut st = CliStatus {
        cli,
        installed: false,
        signed_in: false,
        detail: String::new(),
    };
    let Some(bin) = cli.locate() else {
        st.detail = format!("not installed ({})", cli.install_hint());
        return st;
    };
    st.installed = true;
    let args: &[&str] = match cli {
        Cli::Claude => &["auth", "status"],
        Cli::Codex => &["login", "status"],
    };
    let out = command(&bin).args(args).stdin(Stdio::null()).output();
    let out = match tokio::time::timeout(Duration::from_secs(20), out).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            st.detail = format!("could not run {}: {e}", cli.program());
            return st;
        }
        Err(_) => {
            st.detail = "status check timed out".into();
            return st;
        }
    };
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (st.signed_in, st.detail) = parse_status(cli, out.status.success(), &text);
    st
}

fn parse_status(cli: Cli, ok: bool, text: &str) -> (bool, String) {
    match cli {
        Cli::Claude => match serde_json::from_str::<Value>(text.trim()) {
            Ok(v) if v["loggedIn"].as_bool() == Some(true) => (
                true,
                v["authMethod"].as_str().unwrap_or("signed in").to_string(),
            ),
            _ => (false, "not signed in".into()),
        },
        Cli::Codex => {
            let line = text
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("")
                .trim();
            if ok && line.to_lowercase().starts_with("logged in") {
                let how = line.split(" using ").nth(1).unwrap_or("signed in");
                (true, how.to_string())
            } else {
                (false, "not signed in".into())
            }
        }
    }
}

/// A model reached through a vendor CLI.
pub struct CliModel {
    cli: Cli,
    model: String,
    program: Option<PathBuf>,
    calls: AtomicU64,
}

impl CliModel {
    pub fn new(cli: Cli, model: impl Into<String>) -> Self {
        let model = model.into();
        CliModel {
            cli,
            model: if model.trim().is_empty() {
                cli.default_model().into()
            } else {
                model
            },
            program: None,
            calls: AtomicU64::new(0),
        }
    }

    /// Run this executable instead of looking the CLI up (tests, unusual installs).
    pub fn with_program(mut self, path: impl Into<PathBuf>) -> Self {
        self.program = Some(path.into());
        self
    }

    fn program(&self) -> Result<PathBuf> {
        self.program
            .clone()
            .or_else(|| self.cli.locate())
            .ok_or_else(|| {
                ProviderError::MissingCredential(format!(
                    "the {} CLI is not installed ({})",
                    self.cli.program(),
                    self.cli.install_hint()
                ))
            })
    }

    async fn run(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> Result<Completion> {
        let sys = protocol(Some(self.cli), system, tools, false);
        let input = |messages: &[Message]| match self.cli {
            Cli::Claude => render_transcript(messages),
            Cli::Codex => format!("{sys}\n\n{}", render_transcript(messages)),
        };
        let first = self.exec(Mode::OneShot, &sys, input(messages)).await?;
        if !(first.gave_up && !tools.is_empty()) {
            return Ok(first.completion);
        }
        let mut again = messages.to_vec();
        again.push(Message::user(GAVE_UP));
        let second = self.exec(Mode::OneShot, &sys, input(&again)).await?;
        Ok(second.after(&first))
    }

    /// Run the CLI once. `sys` is the full system prompt (Claude takes it as a flag; Codex
    /// reads it from `input`). Returns the completion and the vendor's session id when it
    /// reported one.
    async fn exec(&self, mode: Mode<'_>, sys: &str, input: String) -> Result<Exec> {
        let bin = self.program()?;
        let dir = work_dir()?;
        let (mut cmd, out_file) = match self.cli {
            Cli::Claude => {
                let mut c = command(&bin);
                c.args([
                    "-p",
                    "--output-format",
                    "json",
                    "--tools",
                    "",
                    "--strict-mcp-config",
                ])
                .args(["--setting-sources", ""]);
                match mode {
                    Mode::OneShot => {
                        c.arg("--no-session-persistence");
                    }
                    Mode::Open(id) => {
                        c.args(["--session-id", id]);
                    }
                    Mode::Resume(id) => {
                        c.args(["--resume", id]);
                    }
                }
                c.args(["--model", &self.model, "--system-prompt", sys])
                    .args(["--json-schema", REPLY_SCHEMA]);
                (c, None)
            }
            Cli::Codex => {
                let n = self.calls.fetch_add(1, Ordering::Relaxed);
                let tag = format!("{}-{n}", std::process::id());
                let schema = dir.join(format!("schema-{tag}.json"));
                std::fs::write(&schema, REPLY_SCHEMA).map_err(io_err)?;
                let mut c = command(&bin);
                match mode {
                    Mode::Resume(id) => {
                        // `resume` takes no sandbox flag; the config key does the same.
                        c.args(["exec", "resume", id, "--json", "--skip-git-repo-check"])
                            .args(["--ignore-rules", "--ignore-user-config"])
                            .args(["-c", "sandbox_mode=\"read-only\""]);
                    }
                    Mode::OneShot | Mode::Open(_) => {
                        c.args(["exec", "--json", "--skip-git-repo-check"]);
                        if matches!(mode, Mode::OneShot) {
                            c.arg("--ephemeral");
                        }
                        c.args(["--ignore-rules", "--ignore-user-config", "-s", "read-only"])
                            .arg("-C")
                            .arg(&dir);
                    }
                }
                c.arg("--output-schema").arg(&schema);
                if self.model != "default" {
                    c.args(["-m", &self.model]);
                }
                c.arg("-");
                (c, Some(schema))
            }
        };
        cmd.current_dir(&dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let result = async {
            let mut child = cmd
                .spawn()
                .map_err(|e| self.api(format!("could not start: {e}")))?;
            if let Some(mut w) = child.stdin.take() {
                w.write_all(input.as_bytes()).await.map_err(io_err)?;
            }
            match tokio::time::timeout(CALL_TIMEOUT, child.wait_with_output()).await {
                Ok(r) => r.map_err(io_err),
                Err(_) => Err(self.api(format!("no reply within {}s", CALL_TIMEOUT.as_secs()))),
            }
        }
        .await;
        if let Some(f) = out_file {
            let _ = std::fs::remove_file(f);
        }
        let out = result?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        let parsed = match self.cli {
            Cli::Claude => parse_claude(&stdout),
            Cli::Codex => parse_codex(&stdout),
        };
        match parsed {
            Ok(p) => {
                let c = self.completion(&p.reply, p.usage)?;
                // Claude Code refused calls to host tools the model made as native ones, and the
                // model then answered without asking the host for anything: it gave up.
                let gave_up = self.cli == Cli::Claude && p.turns > 2 && c.tool_calls.is_empty();
                Ok(Exec {
                    completion: c,
                    vendor: p.vendor,
                    gave_up,
                })
            }
            Err(detail) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                let tail = stderr.trim().lines().last().unwrap_or("");
                let detail = if out.status.success() || tail.is_empty() {
                    detail
                } else {
                    format!("{detail} ({tail})")
                };
                Err(self.classify(self.signed_out_hint(detail)))
            }
        }
    }

    /// A usage or rate limit is temporary: the account recovers, so the call may be retried
    /// after a wait. Anything else stays a plain error.
    fn classify(&self, detail: String) -> ProviderError {
        let d = detail.to_lowercase();
        if [
            "rate limit",
            "usage limit",
            "too many requests",
            "overloaded",
            "429",
        ]
        .iter()
        .any(|k| d.contains(k))
        {
            ProviderError::Overloaded {
                provider: self.cli.name().into(),
                detail,
            }
        } else {
            self.api(detail)
        }
    }

    fn api(&self, detail: String) -> ProviderError {
        ProviderError::Api {
            provider: self.cli.name().into(),
            detail,
        }
    }

    fn signed_out_hint(&self, detail: String) -> String {
        let d = detail.to_lowercase();
        if [
            "login",
            "log in",
            "logged in",
            "sign in",
            "authenticat",
            "unauthorized",
        ]
        .iter()
        .any(|k| d.contains(k))
        {
            format!("{detail}. Sign in with `{}`", self.cli.login_command())
        } else {
            detail
        }
    }

    fn completion(&self, reply: &Value, usage: Usage) -> Result<Completion> {
        let text = reply["text"].as_str().unwrap_or("").to_string();
        let mut calls = Vec::new();
        if reply["action"].as_str() != Some("reply") {
            let base = self.calls.fetch_add(1, Ordering::Relaxed);
            for (i, c) in reply["tool_calls"]
                .as_array()
                .into_iter()
                .flatten()
                .enumerate()
            {
                let name = c["name"].as_str().unwrap_or("").trim().to_string();
                if name.is_empty() {
                    continue;
                }
                let raw = c["arguments"].as_str().unwrap_or("").trim();
                let arguments = if raw.is_empty() {
                    json!({})
                } else {
                    serde_json::from_str(raw).map_err(|e| ProviderError::Malformed {
                        provider: self.cli.name().into(),
                        detail: format!("arguments for {name} are not JSON: {e}"),
                    })?
                };
                calls.push(ToolCall {
                    id: format!("{}_{base}_{i}", self.cli.name()),
                    name,
                    arguments,
                });
            }
        }
        Ok(Completion {
            content: text,
            tool_calls: calls,
            usage,
        })
    }
}

#[async_trait]
impl Provider for CliModel {
    async fn complete(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> Result<Completion> {
        self.run(system, messages, tools).await
    }

    fn describe(&self) -> String {
        format!("{} {}", self.cli.name(), self.model)
    }

    fn model_id(&self) -> String {
        format!("{}:{}", self.cli.name(), self.model)
    }
}

/// One CLI run.
struct Exec {
    completion: Completion,
    vendor: Option<String>,
    /// The model answered without asking for any tool after the CLI refused tools it tried
    /// to call natively (Claude Code reports more than 2 internal turns).
    gave_up: bool,
}

impl Exec {
    /// This run's completion, with the usage of `earlier` (the run it repeats) added.
    fn after(self, earlier: &Exec) -> Completion {
        let (a, b) = (earlier.completion.usage, self.completion.usage);
        let mut c = self.completion;
        c.usage = Usage {
            prompt_tokens: a.prompt_tokens + b.prompt_tokens,
            completion_tokens: a.completion_tokens + b.completion_tokens,
            measured: a.measured && b.measured,
            cached_tokens: a.cached_tokens + b.cached_tokens,
            cache_write_tokens: a.cache_write_tokens + b.cache_write_tokens,
        };
        c
    }
}

/// Sent once when a model gave up after trying host tools as native ones. Seen with Claude
/// Haiku in 1 of 3 live runs of `examples/agent-team` (`bench/RESULTS.md`).
const GAVE_UP: &str = "Your last answer gave up because calling host tools directly failed. Host \
tools are never called directly. Answer again: to use a tool, return action \"call_tools\" with \
the call in tool_calls.";

/// [`GAVE_UP`] as the next message of a vendor session.
fn correction() -> String {
    format!(
        "<new_messages>\n<user>\n{GAVE_UP}\n</user>\n</new_messages>\nContinue as the assistant with one action.\n"
    )
}

/// How one CLI run relates to a vendor session.
#[derive(Clone, Copy)]
enum Mode<'a> {
    /// No session: nothing is kept (the stateless provider).
    OneShot,
    /// Start a session. Claude takes the id from ARK; Codex picks its own and reports it.
    Open(&'a str),
    /// Continue a session by its vendor id.
    Resume(&'a str),
}

#[async_trait]
impl SessionBackend for CliModel {
    fn label(&self) -> String {
        Provider::describe(self)
    }

    fn model(&self) -> String {
        Provider::model_id(self)
    }

    fn account(&self) -> String {
        self.cli.name().to_string()
    }

    async fn open(
        &self,
        system: &str,
        tools: &[ToolDef],
        messages: &[Message],
    ) -> Result<(String, Completion)> {
        let sys = protocol(Some(self.cli), system, tools, true);
        let id = new_session_id();
        let input = match self.cli {
            Cli::Claude => render_transcript(messages),
            Cli::Codex => format!("{sys}\n\n{}", render_transcript(messages)),
        };
        let first = self.exec(Mode::Open(&id), &sys, input).await?;
        let vendor = match self.cli {
            Cli::Claude => first.vendor.clone().unwrap_or(id),
            Cli::Codex => first
                .vendor
                .clone()
                .ok_or_else(|| self.api("codex reported no thread id".into()))?,
        };
        if !(first.gave_up && !tools.is_empty()) {
            return Ok((vendor, first.completion));
        }
        let second = self.exec(Mode::Resume(&vendor), &sys, correction()).await?;
        Ok((vendor, second.after(&first)))
    }

    async fn discard(&self, vendor: &str) {
        for f in self.session_files(vendor) {
            let _ = std::fs::remove_file(&f);
        }
    }

    async fn resume(&self, vendor: &str, base: Base<'_>, delta: &Delta) -> Result<Completion> {
        // Claude records the system prompt with the session; it is passed again unchanged so
        // a CLI without that record behaves the same.
        let sys = protocol(Some(self.cli), base.system, base.tools, true);
        let first = self
            .exec(
                Mode::Resume(vendor),
                &sys,
                render_delta_for(Some(self.cli), delta),
            )
            .await?;
        let offered = delta
            .tools_now
            .as_ref()
            .map(|t| !t.is_empty())
            .unwrap_or(!base.tools.is_empty());
        if !(first.gave_up && offered) {
            return Ok(first.completion);
        }
        let second = self.exec(Mode::Resume(vendor), &sys, correction()).await?;
        Ok(second.after(&first))
    }
}

impl CliModel {
    /// Where the vendor keeps the file of one of ARK's sessions. Claude Code keeps a session
    /// in `<config>/projects/<the working directory, every other character a dash>/<id>.jsonl`;
    /// Codex in `<home>/sessions/<year>/<month>/<day>/rollout-<time>-<id>.jsonl`. ARK only ever
    /// looks in its own working directory's folder (Claude) or for the exact id (Codex).
    fn session_files(&self, vendor: &str) -> Vec<PathBuf> {
        // Only an id the vendor could have made; never a path or a pattern.
        if !valid_session_id(vendor) {
            return Vec::new();
        }
        let home = std::env::var_os("HOME").map(PathBuf::from);
        match self.cli {
            Cli::Claude => {
                let config = std::env::var_os("CLAUDE_CONFIG_DIR")
                    .map(PathBuf::from)
                    .or_else(|| home.map(|h| h.join(".claude")));
                let (Some(config), Ok(dir)) = (config, work_dir()) else {
                    return Vec::new();
                };
                let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
                vec![claude_session_file(&config, &dir, vendor)]
            }
            Cli::Codex => {
                let codex = std::env::var_os("CODEX_HOME")
                    .map(PathBuf::from)
                    .or_else(|| home.map(|h| h.join(".codex")));
                let Some(codex) = codex else {
                    return Vec::new();
                };
                codex_session_files(&codex.join("sessions"), vendor)
            }
        }
    }
}

/// Claude Code's file for session `id` started in `cwd`.
fn claude_session_file(config: &Path, cwd: &Path, id: &str) -> PathBuf {
    let folder: String = cwd
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    config
        .join("projects")
        .join(folder)
        .join(format!("{id}.jsonl"))
}

/// A vendor session id as Claude Code and Codex make them: hex digits and dashes.
fn valid_session_id(id: &str) -> bool {
    id.len() >= 8 && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

/// Codex's files for thread `id`: `rollout-*-<id>.jsonl` under `sessions/<y>/<m>/<d>/`.
fn codex_session_files(sessions: &Path, id: &str) -> Vec<PathBuf> {
    if !valid_session_id(id) {
        return Vec::new();
    }
    let suffix = format!("-{id}.jsonl");
    let mut out = Vec::new();
    let mut dirs = vec![(sessions.to_path_buf(), 0)];
    while let Some((dir, depth)) = dirs.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() && depth < 3 {
                dirs.push((p, depth + 1));
            } else if e
                .file_name()
                .to_string_lossy()
                .strip_prefix("rollout-")
                .is_some_and(|n| n.ends_with(&suffix))
            {
                out.push(p);
            }
        }
    }
    out
}

/// The session backend for a model spec's provider and model, when it is a subscription
/// CLI (`claude`, `codex`).
pub fn session_backend(provider: &str, model: &str) -> Option<Arc<dyn SessionBackend>> {
    let cli = match provider {
        "claude" => Cli::Claude,
        "codex" => Cli::Codex,
        _ => return None,
    };
    Some(Arc::new(CliModel::new(cli, model)))
}

/// A version-4 UUID, as Claude Code requires for `--session-id`.
fn new_session_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// A command with the executable's own directory first on `PATH`: Codex is a Node script,
/// and `node` usually sits next to it.
fn command(bin: &Path) -> tokio::process::Command {
    let mut c = tokio::process::Command::new(bin);
    if let Some(dir) = bin.parent() {
        let mut paths = vec![dir.to_path_buf()];
        if let Some(p) = std::env::var_os("PATH") {
            paths.extend(std::env::split_paths(&p));
        }
        if let Ok(joined) = std::env::join_paths(paths) {
            c.env("PATH", joined);
        }
    }
    c
}

/// An empty directory to run in, so the CLI picks up no project files or instructions.
fn work_dir() -> Result<PathBuf> {
    let d = std::env::temp_dir().join("ark-cli-model");
    std::fs::create_dir_all(&d).map_err(io_err)?;
    Ok(d)
}

fn io_err(e: std::io::Error) -> ProviderError {
    ProviderError::Api {
        provider: "cli".into(),
        detail: e.to_string(),
    }
}

/// The system prompt the CLI sees: the reply protocol, ARK's tools, then the app's own prompt.
pub fn protocol_prompt(system: &str, tools: &[ToolDef]) -> String {
    protocol(None, system, tools, false)
}

/// [`protocol_prompt`] for a vendor session that later requests continue with only what is
/// new.
pub fn session_prompt(system: &str, tools: &[ToolDef]) -> String {
    protocol(None, system, tools, true)
}

/// How the reply protocol opens. Claude Code returns a structured reply through its own
/// `StructuredOutput` tool, and a model told only that it "has no tools" first tries to call
/// a host tool by name (measured: 4 internal turns and 3,049 input tokens for one delegation
/// with the generic wording, 2 turns and 1,384 tokens with this one).
fn opening(cli: Option<Cli>) -> &'static str {
    match cli {
        Some(Cli::Claude) => {
            "You are the model behind a host application's agent. Your only tool is \
StructuredOutput: answer every turn with exactly one StructuredOutput call and nothing else. The \
host owns the tools listed below and runs them for you. They are not tools of yours; to use one, \
name it in the tool_calls field of your StructuredOutput call. Every tool listed is available \
now; no loading step is needed.\n\n"
        }
        _ => {
            "You are the model behind a host application's agent. The host owns the tools listed \
below and runs them. You have no tools of your own and must not try to run anything yourself. \
The host tools are not tools you can call directly: calling one by name fails. You use them only \
by putting them in tool_calls of your structured answer. Every tool listed below is available \
now; no loading step is needed for it.\n\n"
        }
    }
}

fn protocol(cli: Option<Cli>, system: &str, tools: &[ToolDef], session: bool) -> String {
    let mut s = String::from(opening(cli));
    s.push_str(
        "Answer every turn with exactly one action:\n\
- \"call_tools\": put one or more calls in tool_calls (name, and arguments as a JSON object \
string). text may hold a short note. The host runs the calls and sends the results back.\n\
- \"reply\": your answer to the person goes in text, in Markdown. tool_calls must be empty.\n\n\
Earlier results appear in the transcript as <tool_result>. Never repeat a call whose result \
you already have.\n",
    );
    if session {
        s.push_str(
            "\nThis conversation continues over several requests. Each later request carries only \
what is new, in <new_messages>. A <host_update> replaces the tool list or the application's \
instructions it names.\n",
        );
    }
    if tools.is_empty() {
        s.push_str("\nNo host tools are available in this turn. Use \"reply\".\n");
    } else {
        s.push_str(tools_heading(cli));
        for t in tools {
            s.push_str(&tool_entry(cli, t));
        }
    }
    if !system.trim().is_empty() {
        s.push_str("\n# Instructions from the application\n\n");
        s.push_str(system.trim());
        s.push('\n');
    }
    s
}

/// The conversation as tagged text, ending with the cue to continue.
pub fn render_transcript(messages: &[Message]) -> String {
    let mut s = String::from("<transcript>\n");
    for m in messages {
        render_message(&mut s, m);
    }
    s.push_str("</transcript>\nContinue as the assistant with one action.\n");
    s
}

/// The heading of the tool list.
fn tools_heading(cli: Option<Cli>) -> &'static str {
    match cli {
        Some(Cli::Claude) => "\n# Host tools (use them only inside StructuredOutput.tool_calls)\n",
        _ => "\n# Host tools\n",
    }
}

/// One tool in the list. For Claude Code a bullet: under a Markdown heading of its own with
/// "Parameters (JSON Schema)", a tool reads as a native one, and Haiku tried to call it,
/// was refused, and gave up the task (2 of 2 runs; 4 internal turns, 5.5k input tokens).
/// As a bullet: the right structured call, 2 turns, 1.5k tokens. Sonnet is the same either
/// way. Measured with `claude -p`, `bench/RESULTS.md`.
fn tool_entry(cli: Option<Cli>, t: &ToolDef) -> String {
    match cli {
        Some(Cli::Claude) => format!(
            "- `{}`: {}. Arguments: {}\n",
            t.name,
            t.description.trim().trim_end_matches('.'),
            t.parameters
        ),
        _ => format!(
            "\n## {}\n{}\nParameters (JSON Schema): {}\n",
            t.name,
            t.description.trim(),
            t.parameters
        ),
    }
}

/// What a resumed session has not seen: host updates, then the new messages.
pub fn render_delta(delta: &Delta) -> String {
    render_delta_for(None, delta)
}

fn render_delta_for(cli: Option<Cli>, delta: &Delta) -> String {
    let mut s = String::new();
    if delta.tools_now.is_some() || !delta.new_tools.is_empty() {
        s.push_str("<host_update>\n");
        if let Some(names) = &delta.tools_now {
            if names.is_empty() {
                s.push_str("No host tools are available now. Use \"reply\".\n");
            } else {
                s.push_str(&format!(
                    "Host tools available now: {}.\n",
                    names.join(", ")
                ));
            }
        }
        for t in &delta.new_tools {
            s.push_str(&tool_entry(cli, t));
        }
        s.push_str("</host_update>\n");
    }
    if let Some(sys) = &delta.system {
        s.push_str(
            "<host_update>\nThe application's instructions changed. These replace the earlier ones:\n\n",
        );
        s.push_str(sys.trim());
        s.push_str("\n</host_update>\n");
    }
    s.push_str("<new_messages>\n");
    for m in &delta.messages {
        render_message(&mut s, m);
    }
    s.push_str("</new_messages>\nContinue as the assistant with one action.\n");
    s
}

fn render_message(s: &mut String, m: &Message) {
    match m.role {
        Role::System => s.push_str(&format!("<system>\n{}\n</system>\n", m.content)),
        Role::User => s.push_str(&format!("<user>\n{}\n</user>\n", m.content)),
        Role::Assistant => {
            s.push_str("<assistant>\n");
            if !m.content.trim().is_empty() {
                s.push_str(m.content.trim());
                s.push('\n');
            }
            for c in &m.tool_calls {
                s.push_str(&format!(
                    "<tool_call id=\"{}\" name=\"{}\">{}</tool_call>\n",
                    c.id, c.name, c.arguments
                ));
            }
            s.push_str("</assistant>\n");
        }
        Role::Tool => s.push_str(&format!(
            "<tool_result id=\"{}\" name=\"{}\">\n{}\n</tool_result>\n",
            m.tool_call_id.as_deref().unwrap_or(""),
            m.tool_name.as_deref().unwrap_or(""),
            m.content
        )),
    }
}

/// `claude -p --output-format json`: one result object.
fn parse_claude(stdout: &str) -> std::result::Result<Parsed, String> {
    let v: Value = serde_json::from_str(stdout.trim())
        .map_err(|_| format!("unexpected output: {}", clip(stdout)))?;
    if v["is_error"].as_bool() == Some(true) {
        return Err(v["result"]
            .as_str()
            .unwrap_or("the CLI reported an error")
            .to_string());
    }
    let reply = match &v["structured_output"] {
        Value::Object(_) => v["structured_output"].clone(),
        _ => v["result"]
            .as_str()
            .and_then(|r| serde_json::from_str(r).ok())
            .ok_or_else(|| {
                format!(
                    "reply was not structured: {}",
                    clip(v["result"].as_str().unwrap_or(""))
                )
            })?,
    };
    let u = &v["usage"];
    let n = |k: &str| u[k].as_u64().unwrap_or(0) as u32;
    let cached = n("cache_read_input_tokens");
    let written = n("cache_creation_input_tokens");
    let usage = Usage::measured(n("input_tokens") + cached + written, n("output_tokens"))
        .with_cached(cached)
        .with_cache_write(written);
    let session = v["session_id"].as_str().map(str::to_string);
    Ok(Parsed {
        reply,
        usage,
        vendor: session,
        turns: v["num_turns"].as_u64().unwrap_or(1) as u32,
    })
}

/// A CLI reply, parsed.
struct Parsed {
    reply: Value,
    usage: Usage,
    vendor: Option<String>,
    /// Internal turns the CLI took: 2 for one structured reply, more when the model first
    /// tried tools the CLI refused (Claude Code).
    turns: u32,
}

/// `codex exec --json`: JSONL events; the last agent message is the reply.
fn parse_codex(stdout: &str) -> std::result::Result<Parsed, String> {
    let mut reply = None;
    let mut thread = None;
    let mut usage = Usage::default();
    let mut failure = None;
    for line in stdout.lines() {
        let Ok(ev) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match ev["type"].as_str() {
            Some("thread.started") => {
                thread = ev["thread_id"].as_str().map(str::to_string);
            }
            Some("item.completed") if ev["item"]["type"] == "agent_message" => {
                reply = ev["item"]["text"].as_str().map(str::to_string);
            }
            Some("turn.completed") => {
                let u = &ev["usage"];
                let n = |k: &str| u[k].as_u64().unwrap_or(0) as u32;
                usage = Usage::measured(
                    n("input_tokens"),
                    n("output_tokens") + n("reasoning_output_tokens"),
                )
                .with_cached(n("cached_input_tokens"));
            }
            Some("turn.failed") => {
                failure = ev["error"]["message"].as_str().map(str::to_string);
            }
            Some("error") => {
                failure = ev["message"].as_str().map(str::to_string);
            }
            _ => {}
        }
    }
    if let Some(f) = failure.filter(|_| reply.is_none()) {
        return Err(f);
    }
    let text = reply.ok_or_else(|| format!("no reply: {}", clip(stdout)))?;
    let v = serde_json::from_str(text.trim())
        .map_err(|_| format!("reply was not structured: {}", clip(&text)))?;
    Ok(Parsed {
        reply: v,
        usage,
        vendor: thread,
        turns: 1,
    })
}

fn clip(s: &str) -> String {
    let s = s.trim();
    if s.chars().count() > 300 {
        format!("{}…", s.chars().take(300).collect::<String>())
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake(name: &str, script: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ark-cli-test-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, format!("#!/bin/sh\ncat > /dev/null\n{script}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        p
    }

    fn tools() -> Vec<ToolDef> {
        vec![ToolDef {
            name: "fs.read".into(),
            description: "Read a file".into(),
            parameters: json!({"type":"object","properties":{"path":{"type":"string"}}}),
        }]
    }

    #[test]
    fn prompt_lists_tools_and_app_instructions() {
        let p = protocol_prompt("Be terse.", &tools());
        assert!(p.contains("## fs.read"));
        assert!(p.contains("\"path\""));
        assert!(p.ends_with("Be terse.\n"));
        assert!(protocol_prompt("", &[]).contains("No host tools"));
        assert!(protocol(Some(Cli::Claude), "", &tools(), false)
            .contains("Your only tool is StructuredOutput"));
        assert!(!protocol(Some(Cli::Codex), "", &tools(), false).contains("StructuredOutput"));
    }

    #[test]
    fn transcript_carries_calls_and_results() {
        let call = ToolCall {
            id: "c1".into(),
            name: "fs.read".into(),
            arguments: json!({"path":"a"}),
        };
        let t = render_transcript(&[
            Message::user("read a"),
            Message::assistant_tool_calls(vec![call.clone()]),
            Message::tool_result(&call, "hello"),
        ]);
        assert!(t.contains("<user>\nread a\n</user>"));
        assert!(t.contains(r#"<tool_call id="c1" name="fs.read">{"path":"a"}</tool_call>"#));
        assert!(t.contains("<tool_result id=\"c1\" name=\"fs.read\">\nhello\n</tool_result>"));
    }

    #[test]
    fn status_parsing() {
        assert_eq!(
            parse_status(
                Cli::Claude,
                true,
                r#"{"loggedIn": true, "authMethod": "claude.ai"}"#
            ),
            (true, "claude.ai".into())
        );
        assert!(!parse_status(Cli::Claude, false, r#"{"loggedIn": false}"#).0);
        assert_eq!(
            parse_status(Cli::Codex, true, "Logged in using ChatGPT\n"),
            (true, "ChatGPT".into())
        );
        assert!(!parse_status(Cli::Codex, false, "Not logged in\n").0);
    }

    // The JSON below has the shape of real `claude -p --output-format json` output, trimmed.
    #[tokio::test]
    async fn claude_tool_calls_become_ark_calls() {
        let out = r#"{"type":"result","is_error":false,"result":"","structured_output":{"action":"call_tools","text":"","tool_calls":[{"name":"fs.read","arguments":"{\"path\": \"a\"}"}]},"usage":{"input_tokens":5,"cache_read_input_tokens":100,"cache_creation_input_tokens":20,"output_tokens":7}}"#;
        let bin = fake("claude", &format!("echo '{out}'"));
        let m = CliModel::new(Cli::Claude, "").with_program(bin);
        let c = m
            .complete("sys", &[Message::user("hi")], &tools())
            .await
            .unwrap();
        assert_eq!(c.tool_calls.len(), 1);
        assert_eq!(c.tool_calls[0].name, "fs.read");
        assert_eq!(c.tool_calls[0].arguments, json!({"path":"a"}));
        assert_eq!(c.usage.prompt_tokens, 125);
        assert_eq!(c.usage.cached_tokens, 100);
        assert_eq!(c.usage.cache_write_tokens, 20);
        assert_eq!(m.describe(), "claude sonnet");
    }

    // Event lines have the shape of real `codex exec --json` output.
    #[tokio::test]
    async fn codex_reply_is_text() {
        let script = r#"echo '{"type":"thread.started","thread_id":"t"}'
echo '{"type":"item.completed","item":{"id":"i","type":"agent_message","text":"{\"action\":\"reply\",\"text\":\"done\",\"tool_calls\":[]}"}}'
echo '{"type":"turn.completed","usage":{"input_tokens":50,"cached_input_tokens":10,"output_tokens":4,"reasoning_output_tokens":2}}'"#;
        let m = CliModel::new(Cli::Codex, "").with_program(fake("codex", script));
        let c = m.complete("", &[Message::user("hi")], &[]).await.unwrap();
        assert_eq!(c.content, "done");
        assert!(c.tool_calls.is_empty());
        assert_eq!(c.usage.prompt_tokens, 50);
        assert_eq!(c.usage.completion_tokens, 6);
    }

    /// A fake CLI that records its arguments and stdin next to itself, then prints `out`.
    fn recording(name: &str, out: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("ark-cli-rec-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("log");
        let _ = std::fs::remove_file(&log);
        let p = dir.join(name);
        std::fs::write(
            &p,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" >> {log}\necho '--stdin--' >> {log}\ncat >> {log}\necho '--end--' >> {log}\n{out}\n",
                log = log.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        (p, log)
    }

    #[tokio::test]
    async fn claude_sessions_open_with_an_id_and_resume_with_a_delta() {
        let out = r#"echo '{"is_error":false,"session_id":"S1","structured_output":{"action":"reply","text":"ok","tool_calls":[]},"usage":{"input_tokens":10,"output_tokens":2}}'"#;
        let (bin, log) = recording("claude", out);
        let m = CliModel::new(Cli::Claude, "haiku").with_program(bin);
        let (vendor, c) = SessionBackend::open(&m, "Be terse.", &tools(), &[Message::user("hi")])
            .await
            .unwrap();
        assert_eq!(vendor, "S1");
        assert_eq!(c.content, "ok");
        let delta = Delta {
            messages: vec![Message::user("next")],
            new_tools: vec![ToolDef {
                name: "fs.list".into(),
                description: "List files".into(),
                parameters: json!({"type":"object"}),
            }],
            tools_now: Some(vec!["fs.read".into(), "fs.list".into()]),
            ..Default::default()
        };
        let tools = tools();
        let base = Base {
            system: "Be terse.",
            tools: &tools,
        };
        SessionBackend::resume(&m, "S1", base, &delta)
            .await
            .unwrap();
        let log = std::fs::read_to_string(log).unwrap();
        let (open, resume) = log.split_once("--end--").unwrap();
        assert!(open.contains("--session-id"), "{open}");
        assert!(!open.contains("--no-session-persistence"));
        assert!(open.contains("<user>\nhi\n</user>"));
        assert!(resume.contains("--resume\nS1"), "{resume}");
        assert!(resume.contains("<new_messages>\n<user>\nnext\n</user>"));
        assert!(
            resume.contains("- `fs.list`: List files. Arguments:"),
            "a new tool's schema is sent once"
        );
        assert!(
            !resume.contains("<user>\nhi"),
            "nothing already sent is resent"
        );
    }

    #[tokio::test]
    async fn codex_sessions_report_their_thread_and_resume_it() {
        let out = r#"echo '{"type":"thread.started","thread_id":"T9"}'
echo '{"type":"item.completed","item":{"id":"i","type":"agent_message","text":"{\"action\":\"reply\",\"text\":\"ok\",\"tool_calls\":[]}"}}'
echo '{"type":"turn.completed","usage":{"input_tokens":50,"cached_input_tokens":40,"output_tokens":4}}'"#;
        let (bin, log) = recording("codex", out);
        let m = CliModel::new(Cli::Codex, "").with_program(bin);
        let (vendor, _) = SessionBackend::open(&m, "", &[], &[Message::user("hi")])
            .await
            .unwrap();
        assert_eq!(vendor, "T9");
        let delta = Delta {
            messages: vec![Message::user("more")],
            ..Default::default()
        };
        let base = Base {
            system: "",
            tools: &[],
        };
        SessionBackend::resume(&m, "T9", base, &delta)
            .await
            .unwrap();
        let log = std::fs::read_to_string(log).unwrap();
        let (open, resume) = log.split_once("--end--").unwrap();
        assert!(!open.contains("--ephemeral"), "a session is persisted");
        assert!(open.contains("This conversation continues"));
        assert!(resume.starts_with("\nexec\nresume\nT9"), "{resume}");
        assert!(resume.contains("sandbox_mode"));
        assert!(
            !resume.contains("This conversation continues"),
            "no system prompt again"
        );
    }

    #[tokio::test]
    async fn a_model_that_gave_up_after_refused_native_calls_is_asked_once_more() {
        let gave_up = r#"{"is_error":false,"num_turns":6,"session_id":"S1","structured_output":{"action":"reply","text":"I cannot access the tools.","tool_calls":[]},"usage":{"input_tokens":900,"output_tokens":50}}"#;
        let called = r#"{"is_error":false,"num_turns":2,"session_id":"S1","structured_output":{"action":"call_tools","text":"","tool_calls":[{"name":"fs.read","arguments":"{}"}]},"usage":{"input_tokens":300,"output_tokens":20}}"#;
        // The first run gives up; every later one calls the tool.
        let script = format!(
            "if [ -f \"$(dirname \"$0\")/ran\" ]; then echo '{called}'; else touch \"$(dirname \"$0\")/ran\"; echo '{gave_up}'; fi"
        );
        let (bin, log) = recording("claude-gave-up", &script);
        let _ = std::fs::remove_file(bin.parent().unwrap().join("ran"));
        let m = CliModel::new(Cli::Claude, "haiku").with_program(bin.clone());
        let c = m
            .complete("sys", &[Message::user("read a")], &tools())
            .await
            .unwrap();
        assert_eq!(c.tool_calls.len(), 1, "asked again, it called the tool");
        assert_eq!(c.usage.prompt_tokens, 1200, "both runs are counted");
        let log = std::fs::read_to_string(&log).unwrap();
        assert_eq!(log.matches("--stdin--").count(), 2);
        assert!(log.contains("gave up because calling host tools directly failed"));

        // Without tools a plain reply is just the answer: no second run.
        let _ = std::fs::remove_file(bin.parent().unwrap().join("ran"));
        let c = m
            .complete("sys", &[Message::user("hi")], &[])
            .await
            .unwrap();
        assert!(c.tool_calls.is_empty());
        assert_eq!(c.usage.prompt_tokens, 900);
    }

    #[test]
    fn vendor_session_files_are_found_by_exact_id() {
        let dir = tempfile_dir("files");
        let f = claude_session_file(
            &dir,
            Path::new("/private/var/folders/x/T/ark-cli-model"),
            "abc-1",
        );
        assert!(f.ends_with("projects/-private-var-folders-x-T-ark-cli-model/abc-1.jsonl"));

        let day = dir.join("sessions/2026/10/02");
        std::fs::create_dir_all(&day).unwrap();
        let id = "01a0ff32-1294-7f03-bd66-b92382d230d1";
        let mine = day.join(format!("rollout-2026-10-02T21-37-46-{id}.jsonl"));
        let fork = day.join(format!(
            "rollout-2026-10-02T21-38-00-{id}_01a0ff33-0000.jsonl"
        ));
        let other =
            day.join("rollout-2026-10-02T21-39-00-01a0ff34-1111-2222-3333-444455556666.jsonl");
        for p in [&mine, &fork, &other] {
            std::fs::write(p, "{}").unwrap();
        }
        assert_eq!(codex_session_files(&dir.join("sessions"), id), vec![mine]);
        assert!(codex_session_files(&dir.join("sessions"), "*").is_empty());
        assert!(codex_session_files(&dir.join("sessions"), "").is_empty());
        assert!(!valid_session_id("../../etc"));
        assert!(valid_session_id("cad16645-11da-4e42-b357-96e8fbfe6865"));
    }

    fn tempfile_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ark-cli-files-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn rate_and_usage_limits_are_temporary() {
        let m = CliModel::new(Cli::Claude, "");
        assert!(m
            .classify("Claude usage limit reached".into())
            .is_transient());
        assert!(!m.classify("bad flag".into()).is_transient());
    }

    #[tokio::test]
    async fn bad_arguments_are_malformed_and_retryable() {
        let out = r#"{"is_error":false,"structured_output":{"action":"call_tools","text":"","tool_calls":[{"name":"fs.read","arguments":"{path"}]},"usage":{}}"#;
        let m = CliModel::new(Cli::Claude, "")
            .with_program(fake("claude-bad", &format!("echo '{out}'")));
        let e = m
            .complete("", &[Message::user("hi")], &tools())
            .await
            .unwrap_err();
        assert!(matches!(e, ProviderError::Malformed { .. }));
        assert!(e.is_transient());
    }

    #[tokio::test]
    async fn signed_out_error_says_how_to_sign_in() {
        let out = r#"{"is_error":true,"result":"Not logged in · Please run /login"}"#;
        let m = CliModel::new(Cli::Claude, "")
            .with_program(fake("claude-out", &format!("echo '{out}'; exit 1")));
        let e = m
            .complete("", &[Message::user("hi")], &[])
            .await
            .unwrap_err();
        assert!(e.to_string().contains("claude auth login"), "{e}");
    }
}
