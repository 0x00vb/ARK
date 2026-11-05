//! Subscription models through a vendor's official CLI: Claude Code (`claude`, a Claude
//! subscription) and Codex (`codex`, a ChatGPT plan). ARK never reads their credentials; each
//! completion runs the CLI once and the CLI signs the request with its own login.
//!
//! The CLIs are agents with their own tools. Here those tools are off (`claude --tools ""`;
//! Codex runs read-only and is told it has none), and ARK's tools are described in the system
//! prompt instead. The reply is JSON checked against a schema: either text for the person or a
//! list of tool calls. For Codex the schema is built from the tools of the call, so each call's
//! arguments arrive as an object of the tool's own shape (ADR 0049, `reply.rs`); Claude Code
//! takes arguments as an open object ([`CLAUDE_REPLY_SCHEMA`]). ARK's loop runs the tools, so
//! approvals, traces and budgets work as with any other provider.
//!
//! As a plain [`Provider`], each call starts a process and resends the whole transcript. As a
//! [`SessionBackend`], driven by a [`crate::session::ManagedSession`], a vendor session is
//! opened once (Claude `--session-id`, Codex `exec`) and continued with only what is new
//! (Claude `--resume`, Codex `exec resume`).
//!
//! Claude Code runs with streaming JSON in and out: the reply's text reaches `on_text` while
//! the model writes it, and a vendor session keeps its process between calls ([`crate::live`]),
//! so a call skips the CLI's start-up. Codex runs once per call and its text arrives when the
//! reply is complete.

use crate::codex_app::{AppError, CodexApp};
use crate::live::{LiveProcess, ReadError};
use crate::reply::ReplyShape;
use crate::session::{Base, Delta, SessionBackend};
use crate::stream::JsonTextField;
use crate::{
    Completion, Message, Picture, Provider, ProviderError, Result, Role, TextSink, ToolCall,
    ToolDef, Usage, WebHit,
};
use async_trait::async_trait;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;

/// How long one completion may take before the CLI is stopped.
const CALL_TIMEOUT: Duration = Duration::from_secs(600);

/// Codex models (`codex:<model>`) that refused a typed reply schema in this process: their
/// replies use [`REPLY_SCHEMA`] from then on.
static REFUSED: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// The latest model-specific Codex efforts advertised to a picker in this process. A typed
/// choice is checked against it before the active model changes. No catalog means Codex itself
/// remains the authority, so headless use does not depend on opening a picker first.
type EffortCatalog = HashMap<String, Vec<String>>;
static CODEX_CATALOG: LazyLock<Mutex<Option<EffortCatalog>>> = LazyLock::new(|| Mutex::new(None));

/// The older reply shape for Codex, strict, with tool arguments as a JSON string (an open
/// object is not allowed in OpenAI's strict schemas). Codex now gets a schema built from the
/// tools of the call (ADR 0049); this one is used when that cannot be: no tools, a schema over
/// OpenAI's limits, or a typed schema Codex refused.
pub const REPLY_SCHEMA: &str = r#"{"type":"object","additionalProperties":false,"properties":{"action":{"type":"string","enum":["reply","call_tools"]},"text":{"type":"string"},"tool_calls":{"type":"array","items":{"type":"object","additionalProperties":false,"properties":{"name":{"type":"string"},"arguments":{"type":"string"}},"required":["name","arguments"]}}},"required":["action","text","tool_calls"]}"#;

/// [`REPLY_SCHEMA`] for Claude Code, whose schemas allow an open object: tool arguments are an
/// object. As a string, a program in them is JSON in a string in JSON, and Haiku could not
/// escape it (every try failed, `error_max_structured_output_retries`; `bench/RESULTS.md`).
pub const CLAUDE_REPLY_SCHEMA: &str = r#"{"type":"object","additionalProperties":false,"properties":{"action":{"type":"string","enum":["reply","call_tools"]},"text":{"type":"string"},"tool_calls":{"type":"array","items":{"type":"object","additionalProperties":false,"properties":{"name":{"type":"string"},"arguments":{"type":"object"}},"required":["name","arguments"]}}},"required":["action","text","tool_calls"]}"#;

/// Which vendor CLI.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
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

    /// Effort values accepted by this CLI. Codex's picker replaces these with its live list.
    pub fn efforts(self) -> &'static [&'static str] {
        match self {
            Cli::Claude => &["low", "medium", "high", "xhigh", "max"],
            Cli::Codex => &["low", "medium", "high", "xhigh", "max", "ultra"],
        }
    }

    pub fn model_label(self, model: &str) -> String {
        match (self, model) {
            (Cli::Claude, "sonnet") => "Sonnet".into(),
            (Cli::Claude, "opus") => "Opus".into(),
            (Cli::Claude, "fable") => "Fable".into(),
            (_, "default") => "Default".into(),
            _ => model
                .split('-')
                .map(|part| {
                    let mut chars = part.chars();
                    chars
                        .next()
                        .map(|c| c.to_uppercase().collect::<String>() + chars.as_str())
                        .unwrap_or_default()
                })
                .collect::<Vec<_>>()
                .join("-"),
        }
    }

    /// The input tokens a request may have ([`Provider::context_window`]): Claude models take
    /// 200k; Codex's models take 272k of input.
    pub fn context_window(self) -> u32 {
        match self {
            Cli::Claude => 200_000,
            Cli::Codex => 272_000,
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

    /// Install it for this user only, with no administrator and no screen: a shell line run in
    /// a terminal the person watches (ADR 0052, on another computer).
    pub fn install_line(self) -> &'static str {
        match self {
            Cli::Claude => "curl -fsSL https://claude.ai/install.sh | bash",
            Cli::Codex => "npm install -g --prefix \"$HOME/.local\" @openai/codex",
        }
    }

    /// Its own sign-in where no browser opens: Claude Code prints a link and waits for the code
    /// pasted back; Codex prints a link and a one-time code (device sign-in). Run in a terminal
    /// the person watches; ARK never reads what it stores.
    pub fn sign_in_args(self) -> &'static [&'static str] {
        match self {
            Cli::Claude => &["auth", "login"],
            Cli::Codex => &["login", "--device-auth"],
        }
    }

    /// The program for a provider name (`claude`, `codex`).
    pub fn of(name: &str) -> Option<Cli> {
        [Cli::Claude, Cli::Codex]
            .into_iter()
            .find(|c| c.name() == name)
    }

    pub fn label(self) -> &'static str {
        match self {
            Cli::Claude => "Claude subscription",
            Cli::Codex => "ChatGPT subscription (Codex)",
        }
    }

    /// Find the executable: `PATH`, Claude Code's older own folder (`~/.claude/local`), then
    /// where installers put programs that a GUI app's short `PATH` misses
    /// (`ark_os::find::program_in`; npm's `claude.cmd` on Windows).
    pub fn locate(self) -> Option<PathBuf> {
        if let Ok(p) = std::env::var(self.env_override()) {
            let p = PathBuf::from(p);
            return p.is_file().then_some(p);
        }
        // `~/.local/bin`: where the steps below install on a server, which a service's short
        // PATH does not name.
        let extra = ark_os::dirs::home()
            .map(|h| {
                vec![
                    h.join(".claude").join("local"),
                    h.join(".local").join("bin"),
                ]
            })
            .unwrap_or_default();
        ark_os::find::program_in(self.program(), extra)
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
}

/// Check one CLI. Never fails: problems come back in `detail`.
pub async fn status(cli: Cli) -> CliStatus {
    status_for(cli, None).await
}

pub async fn account_status(cli: Cli, account: &crate::accounts::ModelAccount) -> CliStatus {
    let _lease = match crate::accounts::Lease::setup(account) {
        Ok(lease) => lease,
        Err(error) => {
            return CliStatus {
                cli,
                installed: cli.locate().is_some(),
                signed_in: false,
                detail: error.to_string(),
                identity: None,
            }
        }
    };
    let mut status = status_for(cli, Some(account)).await;
    if status.signed_in {
        if cli == Cli::Codex {
            if let Some(bin) = cli.locate() {
                status.identity = tokio::time::timeout(Duration::from_secs(6), async {
                    let mut app =
                        CodexApp::start_catalog_for(&bin, env!("CARGO_PKG_VERSION"), Some(account))
                            .await
                            .ok()?;
                    let identity = app.identity().await.ok().flatten();
                    app.stop().await;
                    identity
                })
                .await
                .ok()
                .flatten();
            }
        }
        if let Some(source) = crate::keys::source() {
            if let Err(error) = source.activate_account(account, status.identity.as_deref()) {
                status.signed_in = false;
                status.detail = error.to_string();
            }
        }
    }
    status
}

pub(crate) async fn status_for(
    cli: Cli,
    account: Option<&crate::accounts::ModelAccount>,
) -> CliStatus {
    let _lease = account.map(crate::accounts::Lease::setup).transpose();
    let mut st = CliStatus {
        cli,
        installed: false,
        signed_in: false,
        detail: String::new(),
        identity: None,
    };
    if account.is_some_and(|a| a.provider != cli.name()) {
        st.detail = "This account belongs to another provider.".into();
        return st;
    }
    let Some(bin) = cli.locate() else {
        st.detail = format!("not installed ({})", cli.install_hint());
        return st;
    };
    st.installed = true;
    let args: &[&str] = match cli {
        Cli::Claude => &["auth", "status"],
        Cli::Codex => &["login", "status"],
    };
    let mut cmd = command(&bin);
    if let Some(account) = account {
        if let Err(error) = account.setup_command(&mut cmd) {
            st.detail = error.to_string();
            return st;
        }
    }
    let child = match cmd
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(child) => child,
        Err(_) => {
            st.detail = "Could not check this account. Try again.".into();
            return st;
        }
    };
    let mut tree = ark_os::process::Tree::of(child.id());
    let out = child.wait_with_output();
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
    tree.disarm();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (st.signed_in, st.detail) = parse_status(cli, out.status.success(), &text);
    if cli == Cli::Claude && st.signed_in {
        st.identity = serde_json::from_slice::<Value>(&out.stdout)
            .ok()
            .and_then(|v| v["email"].as_str().map(str::to_string));
    }
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
    account: Option<crate::accounts::ModelAccount>,
    model: String,
    effort: Option<String>,
    program: Option<PathBuf>,
    calls: AtomicU64,
    /// Claude Code processes kept for vendor sessions, by session id. Only idle ones are
    /// here: a call takes its process out and puts it back after a complete reply.
    idle: Arc<CliIdle>,
    /// The Codex app server holding this model's threads, when idle (taken the same way).

    /// The app server could not be started: Codex sessions run `codex exec` instead.
    no_app: std::sync::atomic::AtomicBool,
}

#[derive(Default)]
struct CliIdle {
    live: Mutex<HashMap<String, LiveProcess>>,
    app: Mutex<Option<CodexApp>>,
}
#[async_trait]
impl crate::accounts::AccountProcesses for CliIdle {
    async fn stop(&self) {
        let live = std::mem::take(&mut *self.live.lock().unwrap());
        let app = self.app.lock().unwrap().take();
        for (_, process) in live {
            process.stop().await;
        }
        if let Some(app) = app {
            app.stop().await;
        }
    }
}

impl CliModel {
    pub fn new(cli: Cli, model: impl Into<String>) -> Self {
        let requested = model.into();
        let (model, effort) = requested_parts(&requested);
        CliModel {
            cli,
            account: None,
            model: if model.trim().is_empty() {
                cli.default_model().into()
            } else {
                model
            },
            effort,
            program: None,
            calls: AtomicU64::new(0),
            idle: Arc::default(),
            no_app: Default::default(),
        }
    }

    /// Make a CLI model and reject a misspelled effort before changing the active model.
    pub fn try_new(cli: Cli, model: impl Into<String>) -> Result<Self> {
        let requested = model.into();
        let (model, effort) = split_effort(cli, &requested)?;
        let mut this = Self::new(cli, model);
        this.effort = effort;
        Ok(this)
    }

    pub fn for_account(
        cli: Cli,
        model: impl Into<String>,
        account: crate::accounts::ModelAccount,
    ) -> Result<Self> {
        account.check()?;
        if account.provider != cli.name() {
            return Err(ProviderError::Api {
                provider: account.provider.clone(),
                detail: "This account belongs to another provider.".into(),
            });
        }
        let (model, effort) = split_effort_for(cli, &model.into(), Some(&account))?;
        let mut this = Self::new(cli, model);
        this.effort = effort;
        crate::accounts::register_processes(&account.id, this.idle.clone());
        this.account = Some(account);
        Ok(this)
    }

    fn identity(&self) -> &str {
        self.account
            .as_ref()
            .map_or(self.cli.name(), |a| a.id.as_str())
    }

    fn command(&self, bin: &Path) -> Result<tokio::process::Command> {
        let mut cmd = command(bin);
        if let Some(account) = &self.account {
            account.command(&mut cmd)?;
        }
        Ok(cmd)
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
        on_text: Option<TextSink<'_>>,
    ) -> Result<Completion> {
        let sys = protocol(Some(self.cli), system, tools, false);
        let input = |messages: &[Message]| Input::of(render_transcript(messages), messages);
        let first = self
            .exec(Mode::OneShot, &sys, input(messages), tools, on_text)
            .await?;
        if !(first.gave_up && !tools.is_empty()) {
            return Ok(first.completion);
        }
        let mut again = messages.to_vec();
        again.push(Message::user(GAVE_UP));
        let second = self
            .exec(Mode::OneShot, &sys, input(&again), tools, on_text)
            .await?;
        Ok(second.after(&first))
    }

    /// Run the CLI for one completion. `sys` is the full system prompt; `input` what this
    /// call adds; `tools` every tool the model may call in it (Codex's reply schema is built
    /// from them). Returns the completion and the vendor's session id when it reported one.
    async fn exec(
        &self,
        mode: Mode<'_>,
        sys: &str,
        input: Input,
        tools: &[ToolDef],
        on_text: Option<TextSink<'_>>,
    ) -> Result<Exec> {
        let _lease = self
            .account
            .as_ref()
            .map(crate::accounts::Lease::acquire)
            .transpose()?;
        if self.cli == Cli::Claude {
            return self.claude(mode, sys, &input, on_text).await;
        }
        let shape = self.codex_shape(tools);
        if !matches!(mode, Mode::OneShot) && !self.no_app.load(Ordering::Relaxed) {
            if let Some(e) = self.codex_app(mode, sys, &input, &shape, on_text).await {
                return e;
            }
        }
        // `codex exec` takes text only: a picture is left out.
        if !input.pictures.is_empty() {
            tracing::info!("codex exec takes no pictures; sending the text only");
        }
        let input = input.text;
        // `codex exec` reads the system prompt from its input; a resumed thread has it.
        let input = match mode {
            Mode::Resume(_) => input,
            _ => format!("{sys}\n\n{input}"),
        };
        let p = self
            .invoke(mode, Some(sys), input, &shape.schema.to_string(), false)
            .await
            .map_err(|e| self.refused(e, &shape))?;
        let c = self.completion(&p.reply, p.usage, &shape)?;
        if let Some(f) = on_text.filter(|_| !c.content.is_empty()) {
            f(&c.content);
        }
        Ok(Exec {
            completion: c,
            vendor: p.vendor,
            gave_up: false,
        })
    }

    /// One Claude Code completion over streaming JSON. A session's process is reused when one
    /// is running for it, and kept after a complete reply; a one-shot process ends with the
    /// call. Text reaches `on_text` while the model writes it.
    async fn claude(
        &self,
        mode: Mode<'_>,
        sys: &str,
        input: &Input,
        on_text: Option<TextSink<'_>>,
    ) -> Result<Exec> {
        let running = match mode {
            Mode::Resume(id) => self.take_live(id),
            _ => None,
        };
        let mut proc = match running {
            Some(p) => p,
            None => {
                let mut cmd = self.command(&self.program()?)?;
                cmd.args(claude_stream_args(
                    mode,
                    &self.model,
                    self.effort.as_deref(),
                    Some(sys),
                    CLAUDE_REPLY_SCHEMA,
                ))
                .current_dir(work_dir()?);
                LiveProcess::spawn(cmd).map_err(|e| self.api(format!("could not start: {e}")))?
            }
        };
        // Pictures go as image blocks after the text, as in the Messages API.
        let content = if input.pictures.is_empty() {
            json!(input.text)
        } else {
            let mut blocks = vec![json!({ "type": "text", "text": input.text })];
            blocks.extend(input.pictures.iter().map(|p| {
                json!({ "type": "image", "source": { "type": "base64", "media_type": p.media_type, "data": p.base64() } })
            }));
            Value::Array(blocks)
        };
        let message = json!({"type": "user", "message": {"role": "user", "content": content}});
        if let Err(e) = proc.send(&message.to_string()).await {
            return Err(self.stopped(&proc, &e.to_string()));
        }
        let deadline = Instant::now() + CALL_TIMEOUT;
        let mut reply = ClaudeReply::default();
        // When the account is out of capacity, the rate-limit event says when it resets.
        let mut reset: Option<u64> = None;
        let result = loop {
            let wait = deadline.saturating_duration_since(Instant::now());
            let line = match proc.next_line(wait).await {
                Ok(l) => l,
                Err(ReadError::TimedOut) => {
                    proc.stop().await;
                    return Err(self.api(format!("no reply within {}s", CALL_TIMEOUT.as_secs())));
                }
                Err(ReadError::Closed) => return Err(self.stopped(&proc, "it stopped")),
                Err(ReadError::Io(e)) => return Err(self.stopped(&proc, &e.to_string())),
            };
            let Ok(ev) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            match ev["type"].as_str() {
                Some("stream_event") => reply.feed(&ev["event"], on_text),
                Some("rate_limit_event") => {
                    let info = &ev["rate_limit_info"];
                    if let Some(u) = crate::usage::from_claude(info) {
                        if let Some(account) = &self.account {
                            account.record(u);
                        } else {
                            crate::usage::record(u);
                        }
                    }
                    if info["status"].as_str().is_some_and(|s| s != "allowed") {
                        reset = info["resetsAt"].as_u64();
                    }
                }
                Some("result") => break ev,
                _ => {}
            }
        };
        let p = parse_claude_result(&result)
            .map_err(|d| self.classify_with(self.signed_out_hint(d), reset))?;
        let c = self.completion(&p.reply, p.usage, &ReplyShape::open())?;
        // Only a refused native call means the model gave up. Claude Code also adds turns when
        // it makes the model resend a plain-text answer through StructuredOutput.
        let gave_up = reply.refused && c.tool_calls.is_empty();
        if let (Some(f), false) = (on_text, gave_up) {
            // What the stream did not show (text after a refused native call, or a reply not
            // streamed at all) is sent now. A run that gave up shows nothing: it is asked again.
            if let Some(rest) = c.content.strip_prefix(reply.shown.as_str()) {
                if !rest.is_empty() {
                    f(rest);
                }
            }
        }
        let vendor = p.vendor.clone().or(match mode {
            Mode::Open(id) | Mode::Resume(id) => Some(id.to_string()),
            Mode::OneShot => None,
        });
        if let (Some(v), false) = (&vendor, matches!(mode, Mode::OneShot)) {
            self.idle.live.lock().unwrap().insert(v.clone(), proc);
        }
        Ok(Exec {
            completion: c,
            vendor,
            gave_up,
        })
    }

    /// One Codex completion on the app server: `Open` starts a thread with `sys` as its
    /// instructions, `Resume` continues one (loading it first when this process does not hold
    /// it). `None` when the app server cannot be started: the caller runs `codex exec`.
    async fn codex_app(
        &self,
        mode: Mode<'_>,
        sys: &str,
        input: &Input,
        shape: &ReplyShape,
        on_text: Option<TextSink<'_>>,
    ) -> Option<Result<Exec>> {
        let bin = match self.program() {
            Ok(b) => b,
            Err(e) => return Some(Err(e)),
        };
        let idle = self.idle.app.lock().unwrap().take();
        let mut app = match idle.and_then(|mut a| a.running().then_some(a)) {
            Some(a) => a,
            None => {
                match CodexApp::start_for(&bin, env!("CARGO_PKG_VERSION"), self.account.as_ref())
                    .await
                {
                    Ok(a) => a,
                    Err(e) => {
                        tracing::warn!(error = %self.app_error(&e, ""), "codex app-server unavailable; using codex exec");
                        self.no_app.store(true, Ordering::Relaxed);
                        return None;
                    }
                }
            }
        };
        let dir = match work_dir() {
            Ok(d) => d,
            Err(e) => return Some(Err(e)),
        };
        let model = (self.model != "default").then_some(self.model.as_str());
        // No turn has been submitted. An unsupported config overlay must select the
        // already-isolated exec path, not turn a valid model choice into a failed task.
        if app.prepare_host().await.is_err() {
            tracing::warn!("codex host-tool isolation unavailable; using isolated codex exec");
            app.stop().await;
            self.no_app.store(true, Ordering::Relaxed);
            return None;
        }
        let thread = match mode {
            Mode::Open(_) => app.start_thread(&dir, model, sys).await,
            Mode::Resume(id) if app.has_thread(id) => Ok(id.to_string()),
            Mode::Resume(id) => app
                .resume_thread(&dir, model, id)
                .await
                .map(|_| id.to_string()),
            Mode::OneShot => unreachable!("one-shot calls run codex exec"),
        };
        let turn = match thread {
            Ok(t) => app
                .turn(crate::codex_app::TurnRequest {
                    thread_id: &t,
                    input: &input.text,
                    pictures: &input.pictures,
                    schema: &shape.schema,
                    effort: self.effort.as_deref(),
                    wait: CALL_TIMEOUT,
                    on_text,
                })
                .await
                .map(|turn| (t, turn)),
            Err(e) => Err(e),
        };
        let (thread, turn) = match turn {
            Ok(x) => x,
            Err(e) => {
                let err = self.app_error(&e, &app.stderr_tail());
                // A Codex error leaves the process usable; anything else ends it.
                if matches!(e, AppError::Failed(_)) {
                    *self.idle.app.lock().unwrap() = Some(app);
                }
                return Some(Err(self.refused(err, shape)));
            }
        };
        *self.idle.app.lock().unwrap() = Some(app);
        let reply: Value = match serde_json::from_str(turn.text.trim()) {
            Ok(v) => v,
            Err(_) => {
                return Some(Err(ProviderError::Malformed {
                    provider: self.cli.name().into(),
                    detail: format!("reply was not structured: {}", clip(&turn.text)),
                }))
            }
        };
        Some(self.completion(&reply, turn.usage, shape).map(|c| Exec {
            completion: c,
            vendor: Some(thread),
            gave_up: false,
        }))
    }

    /// `tail`: the app server's last line on stderr, which says why it stopped.
    fn app_error(&self, e: &AppError, tail: &str) -> ProviderError {
        let detail = match e {
            AppError::Broken(d) if !tail.is_empty() => format!("codex app-server: {d} ({tail})"),
            AppError::Broken(d) => format!("codex app-server: {d}"),
            AppError::Failed(d) => d.clone(),
            AppError::TimedOut => format!("no reply within {}s", CALL_TIMEOUT.as_secs()),
        };
        self.classify(self.signed_out_hint(detail))
    }

    /// Codex's reply shape for a call offering `tools`: a closed variant per tool, or the older
    /// string form once Codex refused a typed schema for this model.
    fn codex_shape(&self, tools: &[ToolDef]) -> ReplyShape {
        let model = if let Some(account) = &self.account {
            format!("{}#{}", Provider::model_id(self), account.revision)
        } else {
            Provider::model_id(self)
        };
        if REFUSED.lock().unwrap().contains(&model) {
            ReplyShape::legacy()
        } else {
            ReplyShape::typed(tools)
        }
    }

    /// `e`, unless it says Codex refused a typed reply schema (OpenAI's strict check). Then this
    /// model uses the string form from now on, in every lane, and the call fails as
    /// [`ProviderError::Refused`], which is retried. A vendor session reopens after a failure, so nothing
    /// is sent twice to a thread.
    fn refused(&self, e: ProviderError, shape: &ReplyShape) -> ProviderError {
        let detail = e.to_string();
        let refused = detail.contains("invalid_json_schema")
            || detail.contains("Invalid schema for response_format");
        if !shape.typed || !refused {
            return e;
        }
        let model = if let Some(account) = &self.account {
            format!("{}#{}", Provider::model_id(self), account.revision)
        } else {
            Provider::model_id(self)
        };
        tracing::warn!(model = %model, error = %detail, "codex refused the typed reply schema; tool arguments travel as JSON strings from now on");
        let mut r = REFUSED.lock().unwrap();
        if !r.contains(&model) {
            r.push(model);
        }
        ProviderError::Refused {
            provider: self.cli.name().into(),
            detail: format!("the reply schema was refused ({})", clip(&detail)),
        }
    }

    /// The running process of vendor session `id`, if there is one that has not exited.
    fn take_live(&self, id: &str) -> Option<LiveProcess> {
        let mut p = self.idle.live.lock().unwrap().remove(id)?;
        p.running().then_some(p)
    }

    /// The process of a call stopped answering: why, with the CLI's last words.
    fn stopped(&self, proc: &LiveProcess, why: &str) -> ProviderError {
        let tail = proc.stderr_tail();
        let detail = if tail.is_empty() {
            format!("{} {why}", self.cli.program())
        } else {
            format!("{} {why} ({tail})", self.cli.program())
        };
        self.classify(self.signed_out_hint(detail))
    }

    /// One search with the CLI's own web search on, and nothing else: Claude Code's
    /// `WebSearch` tool, Codex `--search`. Always a fresh run, never a session.
    async fn search(&self, query: &str, limit: usize) -> Result<Vec<WebHit>> {
        let _lease = self
            .account
            .as_ref()
            .map(crate::accounts::Lease::acquire)
            .transpose()?;
        let input = crate::search_prompt(query, limit);
        let p = self
            .invoke(Mode::OneShot, None, input, crate::SEARCH_SCHEMA, true)
            .await?;
        Ok(crate::hits_from(&p.reply, limit))
    }

    /// Run the CLI once with a reply shaped by `schema`. `sys`: the system prompt (Claude
    /// takes it as a flag; Codex reads it from `input`). `search`: the CLI's own web search
    /// is the one tool it may use.
    async fn invoke(
        &self,
        mode: Mode<'_>,
        sys: Option<&str>,
        input: String,
        schema: &str,
        search: bool,
    ) -> Result<Parsed> {
        let bin = self.program()?;
        let dir = work_dir()?;
        let (mut cmd, out_file) = match self.cli {
            Cli::Claude => {
                let mut c = self.command(&bin)?;
                let tools = if search { "WebSearch" } else { "" };
                c.args(["-p", "--output-format", "json", "--tools", tools])
                    .args(["--strict-mcp-config", "--setting-sources", ""]);
                if search {
                    c.args(["--allowedTools", "WebSearch"]);
                }
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
                c.args(["--model", &self.model]);
                if let Some(effort) = &self.effort {
                    c.args(["--effort", effort]);
                }
                if let Some(sys) = sys {
                    c.args(["--system-prompt", sys]);
                }
                c.args(["--json-schema", schema]);
                (c, None)
            }
            Cli::Codex => {
                let schema_text = schema;
                // One file per call: models of one process run at once, each call with its
                // own schema.
                let schema = dir.join(format!("schema-{}.json", new_session_id()));
                std::fs::write(&schema, schema_text).map_err(io_err)?;
                let mut c = self.command(&bin)?;
                if search {
                    c.arg("--search");
                }
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
                host_codex_flags(&mut c, search);
                c.arg("--output-schema").arg(&schema);
                if self.model != "default" {
                    c.args(["-m", &self.model]);
                }
                if let Some(effort) = &self.effort {
                    c.args(["-c", &format!("model_reasoning_effort=\"{effort}\"")]);
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
            // A cancelled or timed-out call stops the CLI with what it started.
            let mut tree = ark_os::process::Tree::of(child.id());
            if let Some(mut w) = child.stdin.take() {
                w.write_all(input.as_bytes()).await.map_err(io_err)?;
            }
            match tokio::time::timeout(CALL_TIMEOUT, child.wait_with_output()).await {
                Ok(r) => {
                    tree.disarm();
                    r.map_err(io_err)
                }
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
            Ok(p) => Ok(p),
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

    /// A rate limit or an overloaded service passes within seconds, so the call may be
    /// retried after a wait. A subscription's usage limit lasts until its window resets
    /// (hours): retrying inside a turn cannot help, so it is a plain error that says when the
    /// limit resets. Anything else stays a plain error.
    fn classify(&self, detail: String) -> ProviderError {
        self.classify_with(detail, None)
    }

    /// [`classify`](Self::classify), with the reset time (Unix seconds) the CLI reported
    /// outside the error text.
    fn classify_with(&self, detail: String, reset: Option<u64>) -> ProviderError {
        let d = detail.to_lowercase();
        if d.contains("usage limit") || d.contains("limit reached") {
            return self.api(self.usage_limit(&detail, reset));
        }
        if [
            "rate limit",
            "too many requests",
            "overloaded",
            "429",
            "529",
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

    /// What to tell the person when the account's usage limit is reached. Claude Code ends
    /// the error with `|<reset time>`; its rate-limit event carries the same time.
    fn usage_limit(&self, detail: &str, reset: Option<u64>) -> String {
        let (text, stamp) = match detail.rsplit_once('|') {
            Some((t, n)) => (t.trim(), n.trim().parse::<u64>().ok()),
            None => (detail.trim(), None),
        };
        let mut s = format!("{}: {}", self.cli.label(), text.trim_end_matches('.'));
        if let Some(wait) = stamp.or(reset).and_then(until) {
            s.push_str(&format!(". It resets in {wait}"));
        }
        s.push_str(". Use another model meanwhile (/model).");
        s
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

    /// The completion in a reply shaped by `shape`. Arguments that are not what the shape
    /// promised are a malformed reply: nothing runs with guessed arguments.
    fn completion(&self, reply: &Value, usage: Usage, shape: &ReplyShape) -> Result<Completion> {
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
                let arguments = shape.arguments(&name, &c["arguments"]).map_err(|e| {
                    ProviderError::Malformed {
                        provider: self.cli.name().into(),
                        detail: format!("arguments for {name}: {e}"),
                    }
                })?;
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
    fn host_controls_tools(&self) -> bool {
        self.cli == Cli::Claude
    }
    fn connection_revision(&self) -> u64 {
        self.account.as_ref().map_or(0, |a| a.revision)
    }
    fn for_conversation(&self, _conversation: &str) -> Result<Option<Arc<dyn Provider>>> {
        if let Some(account) = &self.account {
            let current = crate::accounts::get(&account.id)?.ok_or_else(|| {
                self.api("This account was removed. Choose an account in Models.".into())
            })?;
            current.check()?;
            if current.revision != account.revision {
                return Ok(Some(crate::build(
                    &current.id,
                    Some(format!(
                        "{}{}",
                        self.model,
                        effort_suffix(self.effort.as_deref())
                    )),
                )?));
            }
        }
        Ok(None)
    }

    /// Claude Code takes image blocks on its input, Codex's app server image items. (`codex
    /// exec`, the fallback, takes text only.)
    fn reads_pictures(&self) -> bool {
        true
    }
    fn context_window(&self) -> Option<u32> {
        Some(self.cli.context_window())
    }
    /// Claude Code marks one cache breakpoint at the end of the system prompt.
    fn caches_system_prompt_whole(&self) -> bool {
        self.cli == Cli::Claude
    }
    async fn complete(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> Result<Completion> {
        self.run(system, messages, tools, None).await
    }

    async fn complete_streaming(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDef],
        on_text: TextSink<'_>,
    ) -> Result<Completion> {
        self.run(system, messages, tools, Some(on_text)).await
    }

    async fn web_search(&self, query: &str, limit: usize) -> Option<Result<Vec<WebHit>>> {
        Some(self.search(query, limit).await)
    }

    fn describe(&self) -> String {
        format!(
            "{} {}{}",
            self.identity(),
            self.model,
            effort_suffix(self.effort.as_deref())
        )
    }

    fn model_id(&self) -> String {
        format!(
            "{}:{}{}",
            self.identity(),
            self.model,
            effort_suffix(self.effort.as_deref())
        )
    }
}

/// One CLI run.
struct Exec {
    completion: Completion,
    vendor: Option<String>,
    /// The model answered without asking for any tool after the CLI refused tools it tried
    /// to call natively.
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

/// Claude Code's flags for streaming JSON in and out: its own tools off, no project settings,
/// the reply shaped by `schema`.
fn claude_stream_args(
    mode: Mode<'_>,
    model: &str,
    effort: Option<&str>,
    sys: Option<&str>,
    schema: &str,
) -> Vec<String> {
    let mut a: Vec<String> = [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--include-partial-messages",
        "--verbose",
        "--tools",
        "",
        "--strict-mcp-config",
        "--setting-sources",
        "",
    ]
    .map(String::from)
    .to_vec();
    match mode {
        Mode::OneShot => a.push("--no-session-persistence".into()),
        Mode::Open(id) => a.extend(["--session-id".into(), id.into()]),
        Mode::Resume(id) => a.extend(["--resume".into(), id.into()]),
    }
    a.extend(["--model".into(), model.into()]);
    if let Some(effort) = effort {
        a.extend(["--effort".into(), effort.into()]);
    }
    if let Some(sys) = sys {
        a.extend(["--system-prompt".into(), sys.into()]);
    }
    a.extend(["--json-schema".into(), schema.into()]);
    a
}

/// What Claude Code's stream events showed of one reply. The structured reply arrives as the
/// input of its `StructuredOutput` tool, in pieces; its `text` field goes to `on_text` as it
/// is decoded. A native tool call the CLI refuses means the model may give up and be asked
/// again, so from then on nothing more is shown of this run.
#[derive(Default)]
struct ClaudeReply {
    field: Option<JsonTextField>,
    /// The structured output's content block index.
    block: Option<u64>,
    refused: bool,
    /// The text sent to `on_text` so far.
    shown: String,
}

impl ClaudeReply {
    fn feed(&mut self, ev: &Value, on_text: Option<TextSink<'_>>) {
        match ev["type"].as_str() {
            Some("content_block_start") if ev["content_block"]["type"] == "tool_use" => {
                if ev["content_block"]["name"] == "StructuredOutput" {
                    self.block = ev["index"].as_u64();
                    self.field = Some(JsonTextField::new("text"));
                } else {
                    self.refused = true;
                }
            }
            Some("content_block_delta") if ev["delta"]["type"] == "input_json_delta" => {
                if ev["index"].as_u64() != self.block || self.refused {
                    return;
                }
                let (Some(field), Some(f)) = (self.field.as_mut(), on_text) else {
                    return;
                };
                let text = field.push(ev["delta"]["partial_json"].as_str().unwrap_or(""));
                if !text.is_empty() {
                    f(&text);
                    self.shown.push_str(&text);
                }
            }
            _ => {}
        }
    }
}

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
    fn host_controls_tools(&self) -> bool {
        Provider::host_controls_tools(self)
    }
    fn reads_pictures(&self) -> bool {
        true
    }
    fn context_window(&self) -> Option<u32> {
        Some(self.cli.context_window())
    }
    /// Claude Code marks one cache breakpoint at the end of the system prompt.
    fn caches_system_prompt_whole(&self) -> bool {
        self.cli == Cli::Claude
    }
    fn label(&self) -> String {
        Provider::describe(self)
    }

    fn model(&self) -> String {
        Provider::model_id(self)
    }

    fn account(&self) -> String {
        self.identity().to_string()
    }
    fn account_revision(&self) -> u64 {
        self.account.as_ref().map_or(0, |a| a.revision)
    }

    async fn web_search(&self, query: &str, limit: usize) -> Option<Result<Vec<WebHit>>> {
        Some(self.search(query, limit).await)
    }

    async fn open(
        &self,
        system: &str,
        tools: &[ToolDef],
        messages: &[Message],
    ) -> Result<(String, Completion)> {
        self.open_session(system, tools, messages, None).await
    }

    async fn open_streaming(
        &self,
        system: &str,
        tools: &[ToolDef],
        messages: &[Message],
        on_text: TextSink<'_>,
    ) -> Result<(String, Completion)> {
        self.open_session(system, tools, messages, Some(on_text))
            .await
    }

    async fn discard(&self, vendor: &str) {
        let running = self.idle.live.lock().unwrap().remove(vendor);
        if let Some(p) = running {
            p.stop().await;
        }
        let app = {
            let mut a = self.idle.app.lock().unwrap();
            if a.as_ref().is_some_and(|a| a.has_thread(vendor)) {
                a.take()
            } else {
                None
            }
        };
        if let Some(a) = app {
            a.stop().await;
        }
        for f in self.session_files(vendor) {
            let _ = std::fs::remove_file(&f);
        }
    }

    async fn resume(&self, vendor: &str, base: Base<'_>, delta: &Delta) -> Result<Completion> {
        self.resume_session(vendor, base, delta, None).await
    }

    async fn resume_streaming(
        &self,
        vendor: &str,
        base: Base<'_>,
        delta: &Delta,
        on_text: TextSink<'_>,
    ) -> Result<Completion> {
        self.resume_session(vendor, base, delta, Some(on_text))
            .await
    }
}

impl CliModel {
    async fn open_session(
        &self,
        system: &str,
        tools: &[ToolDef],
        messages: &[Message],
        on_text: Option<TextSink<'_>>,
    ) -> Result<(String, Completion)> {
        let sys = protocol(Some(self.cli), system, tools, true);
        let id = new_session_id();
        let input = Input::of(render_transcript(messages), messages);
        let first = self
            .exec(Mode::Open(&id), &sys, input, tools, on_text)
            .await?;
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
        let second = self
            .exec(
                Mode::Resume(&vendor),
                &sys,
                correction().into(),
                tools,
                on_text,
            )
            .await?;
        Ok((vendor, second.after(&first)))
    }

    async fn resume_session(
        &self,
        vendor: &str,
        base: Base<'_>,
        delta: &Delta,
        on_text: Option<TextSink<'_>>,
    ) -> Result<Completion> {
        // Claude records the system prompt with the session; it is passed again unchanged so
        // a CLI without that record behaves the same.
        let sys = protocol(Some(self.cli), base.system, base.tools, true);
        let first = self
            .exec(
                Mode::Resume(vendor),
                &sys,
                Input::of(render_delta_for(Some(self.cli), delta), &delta.messages),
                &delta.tools,
                on_text,
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
        let second = self
            .exec(
                Mode::Resume(vendor),
                &sys,
                correction().into(),
                &delta.tools,
                on_text,
            )
            .await?;
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
        let home = ark_os::dirs::home();
        match self.cli {
            Cli::Claude => {
                let config = if let Some(account) = &self.account {
                    account.directory().ok()
                } else {
                    std::env::var_os("CLAUDE_CONFIG_DIR")
                        .map(PathBuf::from)
                        .or_else(|| home.map(|h| h.join(".claude")))
                };
                let (Some(config), Ok(dir)) = (config, work_dir()) else {
                    return Vec::new();
                };
                let dir = ark_os::dirs::canonicalize(&dir).unwrap_or(dir);
                vec![claude_session_file(&config, &dir, vendor)]
            }
            Cli::Codex => {
                let codex = if let Some(account) = &self.account {
                    account.directory().ok()
                } else {
                    std::env::var_os("CODEX_HOME")
                        .map(PathBuf::from)
                        .or_else(|| home.map(|h| h.join(".codex")))
                };
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
    if provider.contains('~') {
        let account = crate::accounts::get(provider).ok()??;
        let cli = Cli::of(&account.provider)?;
        return CliModel::for_account(cli, model, account)
            .ok()
            .map(|m| Arc::new(m) as Arc<dyn SessionBackend>);
    }
    let cli = match provider {
        "claude" => Cli::Claude,
        "codex" => Cli::Codex,
        _ => return None,
    };
    CliModel::try_new(cli, model)
        .ok()
        .map(|model| Arc::new(model) as Arc<dyn SessionBackend>)
}

fn effort_suffix(effort: Option<&str>) -> String {
    effort.map(|e| format!("@{e}")).unwrap_or_default()
}

fn split_effort(cli: Cli, requested: &str) -> Result<(String, Option<String>)> {
    split_effort_for(cli, requested, None)
}

static ACCOUNT_CATALOG: LazyLock<Mutex<HashMap<String, EffortCatalog>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
fn catalog_key(account: &crate::accounts::ModelAccount) -> String {
    format!("{}:{}", account.id, account.revision)
}

fn split_effort_for(
    cli: Cli,
    requested: &str,
    account: Option<&crate::accounts::ModelAccount>,
) -> Result<(String, Option<String>)> {
    let requested = requested.trim();
    let Some((model, effort)) = requested.rsplit_once('@') else {
        return Ok((requested.to_string(), None));
    };
    if model.is_empty() || effort.is_empty() || !cli.efforts().contains(&effort) {
        return Err(ProviderError::Api {
            provider: cli.name().into(),
            detail: format!(
                "effort `{effort}` is unavailable; choose {}",
                cli.efforts().join(", ")
            ),
        });
    }
    if let (Cli::Codex, Some(account)) = (cli, account) {
        let key = catalog_key(account);
        let known = ACCOUNT_CATALOG.lock().unwrap().contains_key(&key);
        if !known {
            let account = account.clone();
            let listed = std::thread::spawn(move || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .ok()
                    .and_then(|r| r.block_on(codex_models_for(Some(&account))))
            })
            .join()
            .ok()
            .flatten();
            if listed.is_none() {
                return Err(ProviderError::Api {
                    provider: "codex".into(),
                    detail: "Could not verify this account's models. Retry in Models.".into(),
                });
            }
        }
        let supported = ACCOUNT_CATALOG
            .lock()
            .unwrap()
            .get(&key)
            .and_then(|m| m.get(model))
            .is_some_and(|efforts| efforts.iter().any(|e| e == effort));
        if !supported {
            return Err(ProviderError::Api {
                provider: "codex".into(),
                detail: "This account did not advertise that model and effort.".into(),
            });
        }
    } else if cli == Cli::Codex {
        if CODEX_CATALOG.lock().unwrap().is_none() && !load_codex_catalog() {
            return Err(ProviderError::Api {
                provider: cli.name().into(),
                detail:
                    "could not verify that effort for this Codex model; list models and try again"
                        .into(),
            });
        }
        let catalog = CODEX_CATALOG.lock().unwrap();
        let supported = catalog
            .as_ref()
            .and_then(|models| models.get(model))
            .ok_or_else(|| ProviderError::Api {
                provider: cli.name().into(),
                detail: format!("Codex did not advertise model `{model}`"),
            })?;
        if !supported.iter().any(|e| e == effort) {
            return Err(ProviderError::Api {
                provider: cli.name().into(),
                detail: format!(
                    "effort `{effort}` is unavailable for `{model}`; choose {}",
                    supported.join(", ")
                ),
            });
        }
    }
    Ok((model.to_string(), Some(effort.to_string())))
}

fn requested_parts(requested: &str) -> (String, Option<String>) {
    let requested = requested.trim();
    match requested.rsplit_once('@') {
        Some((model, effort)) if !model.is_empty() && !effort.is_empty() => {
            (model.to_string(), Some(effort.to_string()))
        }
        _ => (requested.to_string(), None),
    }
}

fn remember_codex_models(models: &[crate::ModelOption]) {
    *CODEX_CATALOG.lock().unwrap() = Some(
        models
            .iter()
            .map(|model| (model.id.clone(), model.efforts.clone()))
            .collect(),
    );
}

/// A typed headless model spec has no picker to populate the live catalog first. Query on a
/// short-lived thread so construction can keep its synchronous API even inside a Tokio worker.
fn load_codex_catalog() -> bool {
    std::thread::Builder::new()
        .name("ark-codex-models".into())
        .spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .ok()
                .and_then(|runtime| runtime.block_on(codex_models()))
                .is_some()
        })
        .ok()
        .and_then(|thread| thread.join().ok())
        .unwrap_or(false)
}

/// Ask the installed Codex app server for this account's model catalog. Explicit efforts
/// remain unavailable when their advertised support cannot be verified.
pub(crate) async fn codex_models() -> Option<Vec<crate::ModelOption>> {
    codex_models_for(None).await
}

#[cfg(test)]
async fn codex_models_from(bin: &Path, wait: Duration) -> Option<Vec<crate::ModelOption>> {
    codex_models_from_for(bin, wait, None).await
}

pub(crate) async fn codex_models_for(
    account: Option<&crate::accounts::ModelAccount>,
) -> Option<Vec<crate::ModelOption>> {
    codex_models_from_for(&Cli::Codex.locate()?, Duration::from_secs(6), account).await
}

async fn codex_models_from_for(
    bin: &Path,
    wait: Duration,
    account: Option<&crate::accounts::ModelAccount>,
) -> Option<Vec<crate::ModelOption>> {
    let _lease = account
        .map(crate::accounts::Lease::acquire)
        .transpose()
        .ok()?;
    let listed = tokio::time::timeout(wait, async {
        let mut app = CodexApp::start_catalog_for(bin, env!("CARGO_PKG_VERSION"), account)
            .await
            .ok()?;
        let models = app.models().await.ok();
        app.stop().await;
        models
    })
    .await
    .ok()
    .flatten()?;
    if listed.is_empty() {
        return None;
    }
    if let Some(account) = account {
        ACCOUNT_CATALOG.lock().unwrap().insert(
            catalog_key(account),
            listed
                .iter()
                .map(|m| (m.id.clone(), m.efforts.clone()))
                .collect(),
        );
    } else {
        remember_codex_models(&listed);
    }
    Some(listed)
}

/// A version-4 UUID, as Claude Code requires for `--session-id`.
fn new_session_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// A command started the way ARK starts every program (`ark_os::process::command`), with the
/// executable's own directory first on `PATH`: Codex is a Node script, and `node` usually sits
/// next to it.
pub(crate) fn command(bin: &Path) -> tokio::process::Command {
    let mut c = ark_os::process::command(bin);
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

/// How long until Unix time `at`, in words ("2 h 10 min"). `None` when it has passed.
fn until(at: u64) -> Option<String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    let secs = at.checked_sub(now)?;
    let (h, m) = (secs / 3600, (secs % 3600).div_ceil(60));
    Some(match (h, m) {
        (0, 0) => "under a minute".into(),
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
    })
}

/// Host-model calls run tools in ARK. This never configures an ACP worker or writes settings.
pub(crate) fn host_codex_flags(cmd: &mut tokio::process::Command, search: bool) {
    cmd.args([
        "-c",
        "features.shell_tool=false",
        "-c",
        "features.apps=false",
    ]);
    if !search {
        cmd.args(["-c", "web_search=\"disabled\""]);
    }
}

fn io_err(e: std::io::Error) -> ProviderError {
    ProviderError::Api {
        provider: "cli".into(),
        detail: e.to_string(),
    }
}

/// The reply schema Codex gets for a call offering `tools`: one closed variant per tool, its
/// arguments the tool's schema in OpenAI's strict form (ADR 0049). [`REPLY_SCHEMA`] when that
/// cannot be built (no tools, over a limit).
pub fn codex_reply_schema(tools: &[ToolDef]) -> Value {
    ReplyShape::typed(tools).schema
}

pub use crate::reply::strict_problems;

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
    let arguments = match cli {
        Some(Cli::Claude) => "a JSON object",
        _ => "a JSON object, as the reply schema shapes it",
    };
    s.push_str(&format!(
        "Answer every turn with exactly one action:\n\
- \"call_tools\": put one or more calls in tool_calls (name, and arguments as {arguments}). \
text may hold a short note. The host runs the calls and sends the results back.\n\
- \"reply\": your answer to the person goes in text, in Markdown. tool_calls must be empty.\n\n\
Earlier results appear in the transcript as <tool_result>. Never repeat a call whose result \
you already have.\n",
    ));
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

/// What one call sends the CLI: the rendered text, and the pictures of the messages in it.
struct Input {
    text: String,
    pictures: Vec<Picture>,
}

impl Input {
    fn of(text: String, messages: &[Message]) -> Self {
        Input {
            text,
            pictures: messages.iter().flat_map(|m| m.pictures.clone()).collect(),
        }
    }
}

impl From<String> for Input {
    fn from(text: String) -> Self {
        Input {
            text,
            pictures: Vec::new(),
        }
    }
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
            "<tool_result id=\"{}\" name=\"{}\">\n{}{}\n</tool_result>\n",
            m.tool_call_id.as_deref().unwrap_or(""),
            m.tool_name.as_deref().unwrap_or(""),
            m.content,
            if m.pictures.is_empty() {
                ""
            } else {
                "\n(its picture is attached to this message)"
            }
        )),
    }
}

/// `claude -p --output-format json`: one result object.
fn parse_claude(stdout: &str) -> std::result::Result<Parsed, String> {
    let v: Value = serde_json::from_str(stdout.trim())
        .map_err(|_| format!("unexpected output: {}", clip(stdout)))?;
    parse_claude_result(&v)
}

/// Claude Code's result object: the whole output of `--output-format json`, the last event of
/// `stream-json`.
fn parse_claude_result(v: &Value) -> std::result::Result<Parsed, String> {
    if v["is_error"].as_bool() == Some(true) {
        // Some errors (a turn limit, too many retries of the structured reply) have no text,
        // only a subtype: name it, so the cause shows.
        return Err(match (v["result"].as_str(), v["subtype"].as_str()) {
            (Some(r), _) if !r.trim().is_empty() => r.to_string(),
            (_, Some(kind)) => format!("the CLI reported an error ({kind})"),
            _ => "the CLI reported an error".to_string(),
        });
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
    })
}

/// A CLI reply, parsed.
struct Parsed {
    reply: Value,
    usage: Usage,
    vendor: Option<String>,
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

pub async fn refresh_codex_usage(account: Option<&crate::accounts::ModelAccount>) -> Result<()> {
    let _lease = account.map(crate::accounts::Lease::acquire).transpose()?;
    let bin = Cli::Codex
        .locate()
        .ok_or_else(|| ProviderError::MissingCredential("Codex is not installed.".into()))?;
    let id = account.map_or("codex", |a| a.id.as_str());
    let before = crate::usage::usage_of(id).map(|u| u.reported_ms);
    tokio::time::timeout(Duration::from_secs(6), async {
        let app = CodexApp::start_for(&bin, env!("CARGO_PKG_VERSION"), account)
            .await
            .map_err(|_| ProviderError::Api {
                provider: "codex".into(),
                detail: "Could not refresh usage. Try again.".into(),
            })?;
        app.stop().await;
        let refreshed = crate::usage::usage_of(id)
            .is_some_and(|u| !u.windows.is_empty() && Some(u.reported_ms) != before);
        if !refreshed {
            return Err(ProviderError::Api {
                provider: "codex".into(),
                detail: "Usage is unavailable. Try again later.".into(),
            });
        }
        Ok(())
    })
    .await
    .map_err(|_| ProviderError::Api {
        provider: "codex".into(),
        detail: "Usage refresh timed out. Try again.".into(),
    })?
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake CLI that reads all of stdin, then runs `script`.
    #[cfg(unix)]
    fn fake(name: &str, script: &str) -> PathBuf {
        fake_raw(name, &format!("cat > /dev/null\n{script}"))
    }

    /// A fake CLI that runs `script` as it is.
    #[cfg(unix)]
    fn fake_raw(name: &str, script: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ark-cli-test-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        std::fs::write(
            &p,
            format!("#!/bin/sh\n[ \"$1\" = app-server ] && exit 1\n{script}\n"),
        )
        .unwrap();
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

    /// The events Claude Code streams for one structured reply (shapes from a live
    /// `claude -p --output-format stream-json --include-partial-messages` run, trimmed): the
    /// reply in three pieces, then the result.
    #[cfg(unix)]
    fn stream_reply(structured: Value, result: Value) -> String {
        let text = structured.to_string();
        let cut = text.len() / 3;
        let (a, rest) = text.split_at(cut);
        let (b, c) = rest.split_at(cut);
        let mut lines = vec![
            json!({"type":"system","subtype":"init","session_id":result["session_id"]}),
            json!({"type":"stream_event","event":{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","name":"StructuredOutput","input":{}}}}),
        ];
        for piece in [a, b, c] {
            lines.push(json!({"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":piece}}}));
        }
        let mut r = result;
        r["type"] = json!("result");
        r["structured_output"] = structured;
        lines.push(r);
        lines.iter().map(|l| format!("{l}\n")).collect()
    }

    /// A fake `claude` speaking streaming JSON. It logs its arguments once per start and each
    /// line it reads, and answers the n-th line (counted across starts) with `replies[n]`, the
    /// last one again when they run out. Returns the program and its log.
    #[cfg(unix)]
    fn claude_stream(name: &str, replies: &[String]) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("ark-cli-live-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (i, r) in replies.iter().enumerate() {
            std::fs::write(dir.join(format!("reply-{i}")), r).unwrap();
        }
        let last = replies.len() - 1;
        let log = dir.join("log");
        let p = dir.join(name);
        std::fs::write(
            &p,
            format!(
                r#"#!/bin/sh
d=$(dirname "$0")
echo "--start--" >> "$d/log"
printf '%s\n' "$@" >> "$d/log"
while IFS= read -r line; do
  echo "--line--" >> "$d/log"
  printf '%s\n' "$line" >> "$d/log"
  n=$(cat "$d/count" 2>/dev/null || echo 0)
  echo $((n + 1)) > "$d/count"
  [ "$n" -gt {last} ] && n={last}
  cat "$d/reply-$n"
done
"#
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

    #[cfg(unix)]
    fn reply(text: &str) -> Value {
        json!({"action":"reply","text":text,"tool_calls":[]})
    }

    #[cfg(unix)]
    fn usage(session: &str, turns: u32, input: u32) -> Value {
        json!({"is_error":false,"num_turns":turns,"session_id":session,"usage":{"input_tokens":input,"output_tokens":2}})
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

    #[cfg(unix)]
    #[tokio::test]
    async fn claude_tool_calls_become_ark_calls() {
        let out = stream_reply(
            json!({"action":"call_tools","text":"","tool_calls":[{"name":"fs.read","arguments":"{\"path\": \"a\"}"}]}),
            json!({"is_error":false,"result":"","usage":{"input_tokens":5,"cache_read_input_tokens":100,"cache_creation_input_tokens":20,"output_tokens":7}}),
        );
        let (bin, _) = claude_stream("claude-calls", &[out]);
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
    #[cfg(unix)]
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
    #[cfg(unix)]
    fn recording(name: &str, out: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("ark-cli-rec-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("log");
        let _ = std::fs::remove_file(&log);
        let p = dir.join(name);
        std::fs::write(
            &p,
            format!(
                "#!/bin/sh\n[ \"$1\" = app-server ] && exit 1\nprintf '%s\\n' \"$@\" >> {log}\necho '--stdin--' >> {log}\ncat >> {log}\necho '--end--' >> {log}\n{out}\n",
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

    // The flags are the ones a live search was run with (2026-10-03, Claude Code 2.1, Codex
    // 0.153): Claude Code's WebSearch is the one tool it may use; Codex gets `--search`.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_search_turns_on_the_clis_own_web_search_and_nothing_else() {
        let out = r#"echo '{"is_error":false,"structured_output":{"results":[{"title":"Tea","url":"https://tea.example/","snippet":"About tea."},{"title":"Not a page","url":"javascript:x","snippet":""}]},"usage":{"input_tokens":10,"output_tokens":2}}'"#;
        let (bin, log) = recording("claude-search", out);
        let m = CliModel::new(Cli::Claude, "haiku").with_program(bin);
        let hits = Provider::web_search(&m, "green tea", 5)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            hits,
            vec![WebHit {
                title: "Tea".into(),
                url: "https://tea.example/".into(),
                snippet: "About tea.".into()
            }]
        );
        let log = std::fs::read_to_string(log).unwrap();
        assert!(
            log.contains("--tools\nWebSearch\n--strict-mcp-config"),
            "{log}"
        );
        assert!(log.contains("--allowedTools\nWebSearch"), "{log}");
        assert!(log.contains("--no-session-persistence"), "{log}");
        assert!(!log.contains("--system-prompt"), "{log}");
        assert!(log.contains("Search the web for: green tea"), "{log}");

        let script = r#"echo '{"type":"item.completed","item":{"id":"i","type":"agent_message","text":"{\"results\":[{\"title\":\"Tea\",\"url\":\"https://tea.example/\",\"snippet\":\"\"}]}"}}'"#;
        let (bin, log) = recording("codex-search", script);
        let m = CliModel::new(Cli::Codex, "").with_program(bin);
        let hits = Provider::web_search(&m, "green tea", 5)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(hits.len(), 1);
        let log = std::fs::read_to_string(log).unwrap();
        assert!(log.starts_with("--search\nexec\n"), "{log}");
        assert!(log.contains("--ephemeral"), "{log}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn claude_sessions_open_with_an_id_and_resume_with_a_delta() {
        let (bin, log) = claude_stream("claude", &[stream_reply(reply("ok"), usage("S1", 2, 10))]);
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
        assert_eq!(
            log.matches("--start--").count(),
            1,
            "the session kept its process"
        );
        let (open, resume) = log.rsplit_once("--line--").unwrap();
        assert!(open.contains("--session-id"), "{open}");
        assert!(open.contains("--input-format\nstream-json"), "{open}");
        assert!(!open.contains("--no-session-persistence"));
        assert!(open.contains(r"<user>\nhi\n</user>"), "{open}");
        assert!(
            resume.contains(r"<new_messages>\n<user>\nnext\n</user>"),
            "{resume}"
        );
        assert!(
            resume.contains("- `fs.list`: List files. Arguments:"),
            "a new tool's schema is sent once"
        );
        assert!(
            !resume.contains(r"<user>\nhi"),
            "nothing already sent is resent"
        );

        // Without a running process (discarded, or ARK restarted) the session resumes by id.
        SessionBackend::discard(&m, "S1").await;
        SessionBackend::resume(&m, "S1", base, &delta)
            .await
            .unwrap();
        let log = std::fs::read_to_string(m.program().unwrap().with_file_name("log")).unwrap();
        assert_eq!(log.matches("--start--").count(), 2);
        assert!(log
            .rsplit_once("--start--")
            .unwrap()
            .1
            .contains("--resume\nS1"));
    }

    #[cfg(unix)]
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

    /// A fake `codex app-server`: answers the handshake, starts thread T1, and answers each
    /// turn with `reply` in pieces, after sending one request of its own (to be declined).
    /// It logs its arguments and every line it reads.
    #[cfg(unix)]
    fn codex_app(name: &str, reply: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("ark-codex-app-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let text = serde_json::to_string(reply).unwrap();
        let (a, b) = reply.split_at(reply.len() / 2);
        let delta = |d: &str| {
            json!({"method":"item/agentMessage/delta","params":{"threadId":"T1","turnId":"u1","itemId":"i1","delta":d}})
                .to_string()
        };
        let turn = [
            json!({"id":"r1","method":"item/commandExecution/requestApproval","params":{"threadId":"T1"}}).to_string(),
            delta(a),
            delta(b),
            format!(r#"{{"method":"item/completed","params":{{"threadId":"T1","item":{{"type":"agentMessage","id":"i1","text":{text}}}}}}}"#),
            json!({"method":"thread/tokenUsage/updated","params":{"threadId":"T1","turnId":"u1","tokenUsage":{"last":{"inputTokens":900,"cachedInputTokens":800,"outputTokens":12}}}}).to_string(),
            json!({"method":"turn/completed","params":{"threadId":"T1","turn":{"id":"u1","status":"completed"}}}).to_string(),
        ];
        std::fs::write(dir.join("turn"), turn.join("\n") + "\n").unwrap();
        let p = dir.join(name);
        std::fs::write(
            &p,
            r#"#!/bin/sh
d=$(dirname "$0")
echo "--start--" >> "$d/log"
printf '%s\n' "$@" >> "$d/log"
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$d/log"
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*) echo "{\"id\":$id,\"result\":{}}" ;;
    *'"method":"account/rateLimits/read"'*) echo "{\"id\":$id,\"result\":{}}" ;;
    *'"method":"config/read"'*) echo "{\"id\":$id,\"result\":{\"config\":{\"mcp_servers\":{\"unused-server\":{\"command\":\"never-start\"}}}}}" ;;
    *'"method":"model/list"'*)
      case "$line" in
        *'"cursor":"next"'*) echo "{\"id\":$id,\"result\":{\"data\":[{\"id\":\"gpt-5.6-luna\",\"model\":\"gpt-5.6-luna\",\"displayName\":\"GPT-5.6-Luna\",\"hidden\":false,\"defaultReasoningEffort\":\"low\",\"supportedReasoningEfforts\":[{\"reasoningEffort\":\"low\",\"description\":\"Fast\"}]}],\"nextCursor\":null}}" ;;
        *) echo "{\"id\":$id,\"result\":{\"data\":[{\"id\":\"gpt-5.6-terra\",\"model\":\"gpt-5.6-terra\",\"displayName\":\"GPT-5.6-Terra\",\"hidden\":false,\"defaultReasoningEffort\":\"medium\",\"supportedReasoningEfforts\":[{\"reasoningEffort\":\"low\",\"description\":\"Fast\"},{\"reasoningEffort\":\"medium\",\"description\":\"Balanced\"}]}],\"nextCursor\":\"next\"}}" ;;
      esac ;;
    *'"method":"thread/start"'*) echo "{\"id\":$id,\"result\":{\"thread\":{\"id\":\"T1\"}}}" ;;
    *'"method":"thread/resume"'*) echo "{\"id\":$id,\"result\":{}}" ;;
    *'"method":"turn/start"'*) echo "{\"id\":$id,\"result\":{\"turn\":{\"id\":\"u1\"}}}"; cat "$d/turn" ;;
  esac
done
"#,
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        (p, dir.join("log"))
    }

    #[cfg(unix)]
    static CATALOG_FIXTURES: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// Change one RPC response without touching environment or real account configuration.
    #[cfg(unix)]
    fn catalog_fixture(name: &str, method: &str, body: &str) -> (PathBuf, PathBuf) {
        let (bin, log) = codex_app(
            name,
            r#"{"action":"reply","text":"unused","tool_calls":[]}"#,
        );
        let script = std::fs::read_to_string(&bin).unwrap();
        let target = format!("*'\"method\":\"{method}\"'*)");
        let branch = format!("    {target} {body} ;;");
        let script = script.replacen(
            "  case \"$line\" in\n",
            &format!("  case \"$line\" in\n{branch}\n"),
            1,
        );
        let script = script.replace(
            "d=$(dirname \"$0\")",
            "d=$(dirname \"$0\")\necho $$ > \"$d/pid\"",
        );
        std::fs::write(&bin, script + "\n").unwrap();
        (bin, log)
    }

    #[cfg(unix)]
    async fn fixture_pid(bin: &Path, name: &str) -> u32 {
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                if let Ok(pid) = std::fs::read_to_string(bin.with_file_name(name)) {
                    if let Ok(pid) = pid.trim().parse() {
                        return pid;
                    }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("fixture wrote its process id")
    }

    #[cfg(unix)]
    async fn fixture_stopped(pid: u32) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while ark_os::process::alive(pid) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("discovery stopped its process tree");
    }

    #[cfg(unix)]
    const BLOCK_RPC: &str = r#"sleep 60 & echo $! > "$d/descendant"; wait $!"#;

    #[cfg(unix)]
    #[tokio::test]
    async fn catalog_never_waits_for_optional_usage_and_has_no_execution_side_effects() {
        let _fixture = CATALOG_FIXTURES.lock().await;
        let (bin, log) = catalog_fixture("catalog-no-usage", "account/rateLimits/read", BLOCK_RPC);
        let models = codex_models_from(&bin, Duration::from_secs(6))
            .await
            .unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "gpt-5.6-terra");
        assert_eq!(models[0].efforts, ["low", "medium"]);
        let log = std::fs::read_to_string(log).unwrap();
        assert!(
            log.find("\"method\":\"initialize\"").unwrap()
                < log.find("\"method\":\"initialized\"").unwrap()
        );
        assert!(
            log.find("\"method\":\"initialized\"").unwrap()
                < log.find("\"method\":\"model/list\"").unwrap()
        );
        assert_eq!(log.matches("\"method\":\"model/list\"").count(), 2);
        for forbidden in [
            "account/rateLimits/read",
            "config/read",
            "thread/start",
            "turn/start",
        ] {
            assert!(!log.contains(forbidden), "catalog sent {forbidden}");
        }
        fixture_stopped(fixture_pid(&bin, "pid").await).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn catalog_accepts_delayed_required_replies_within_its_deadline() {
        let _fixture = CATALOG_FIXTURES.lock().await;
        let (bin, _) = codex_app("catalog-delayed-required", "unused");
        let script = std::fs::read_to_string(&bin).unwrap().replacen(
            "  case \"$line\" in\n",
            "  sleep 0.1\n  case \"$line\" in\n",
            1,
        );
        std::fs::write(&bin, script).unwrap();
        let models = codex_models_from(&bin, Duration::from_secs(6))
            .await
            .unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].efforts, ["low", "medium"]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn catalog_errors_do_not_become_an_unverified_model_choice() {
        let _fixture = CATALOG_FIXTURES.lock().await;
        for method in ["initialize", "model/list"] {
            let name = format!("catalog-error-{}", method.replace('/', "-"));
            let (bin, log) = catalog_fixture(
                &name,
                method,
                r#"echo "{\"id\":$id,\"error\":{\"code\":-32000,\"message\":\"fixture refusal\"}}""#,
            );
            assert!(codex_models_from(&bin, Duration::from_secs(6))
                .await
                .is_none());
            let log = std::fs::read_to_string(log).unwrap();
            assert!(!log.contains("thread/start") && !log.contains("turn/start"));
            fixture_stopped(fixture_pid(&bin, "pid").await).await;
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn catalog_timeout_stops_handshake_and_model_list_process_trees() {
        let _fixture = CATALOG_FIXTURES.lock().await;
        for method in ["initialize", "model/list"] {
            let name = format!("catalog-timeout-{}", method.replace('/', "-"));
            let (bin, _) = catalog_fixture(&name, method, BLOCK_RPC);
            assert!(tokio::time::timeout(
                Duration::from_secs(8),
                codex_models_from(&bin, Duration::from_secs(6)),
            )
            .await
            .expect("catalog kept its deadline")
            .is_none());
            fixture_stopped(fixture_pid(&bin, "pid").await).await;
            fixture_stopped(fixture_pid(&bin, "descendant").await).await;
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancelling_catalog_discovery_stops_its_process_and_descendant() {
        let _fixture = CATALOG_FIXTURES.lock().await;
        let (bin, _) = catalog_fixture("catalog-cancel", "model/list", BLOCK_RPC);
        let task_bin = bin.clone();
        let task =
            tokio::spawn(async move { codex_models_from(&task_bin, Duration::from_secs(6)).await });
        let child = fixture_pid(&bin, "descendant").await;
        let parent = fixture_pid(&bin, "pid").await;
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        fixture_stopped(parent).await;
        fixture_stopped(child).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn ordinary_start_still_collects_account_usage() {
        let _fixture = CATALOG_FIXTURES.lock().await;
        // The account snapshot is process-global. Run this assertion alone in a child,
        // rather than race other vendor fixtures or mutate the parent's environment.
        const CHILD: &str = "ARK_TEST_CATALOG_USAGE_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "cli::tests::ordinary_start_still_collects_account_usage",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stdout)
            );
            return;
        }
        let (bin, log) = catalog_fixture(
            "catalog-normal-usage",
            "account/rateLimits/read",
            r#"echo "{\"id\":$id,\"result\":{\"rateLimits\":{\"planType\":\"pro\",\"primary\":{\"usedPercent\":37,\"windowDurationMins\":300}}}}""#,
        );
        let app = CodexApp::start(&bin, "test").await.unwrap();
        let usage = crate::usage::usage_of("codex").unwrap();
        assert_eq!(usage.plan.as_deref(), Some("pro"));
        assert_eq!(usage.windows[0].used_percent, 37);
        app.stop().await;
        let log = std::fs::read_to_string(log).unwrap();
        assert!(
            log.find("\"method\":\"initialized\"").unwrap()
                < log.find("account/rateLimits/read").unwrap()
        );
        fixture_stopped(fixture_pid(&bin, "pid").await).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn codex_catalog_includes_live_effort_choices() {
        let (bin, _) = codex_app(
            "codex-models",
            r#"{"action":"reply","text":"ok","tool_calls":[]}"#,
        );
        let mut app = CodexApp::start(&bin, "test").await.unwrap();
        let models = app.models().await.unwrap();
        app.stop().await;
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "gpt-5.6-terra");
        assert_eq!(models[0].label, "GPT-5.6-Terra");
        assert_eq!(models[0].efforts, ["low", "medium"]);
        assert_eq!(models[0].default_effort.as_deref(), Some("medium"));
        assert_eq!(models[1].id, "gpt-5.6-luna");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn codex_sessions_run_on_one_app_server_and_stream() {
        let answer = r#"{"action":"reply","text":"Hello there.","tool_calls":[]}"#;
        let (bin, log) = codex_app("codex", answer);
        let m = CliModel::new(Cli::Codex, "").with_program(bin);
        let shown = Mutex::new(Vec::<String>::new());
        let sink = |t: &str| shown.lock().unwrap().push(t.to_string());
        let (vendor, c) = SessionBackend::open_streaming(
            &m,
            "Be terse.",
            &tools(),
            &[Message::user("hi")],
            &sink,
        )
        .await
        .unwrap();
        assert_eq!(vendor, "T1");
        assert_eq!(c.content, "Hello there.");
        assert_eq!(c.usage, Usage::measured(900, 12).with_cached(800));
        let shown = shown.into_inner().unwrap();
        assert!(shown.len() > 1, "{shown:?}");
        assert_eq!(shown.concat(), "Hello there.");

        let delta = Delta {
            messages: vec![Message::user("next")],
            ..Default::default()
        };
        let tools = tools();
        let base = Base {
            system: "Be terse.",
            tools: &tools,
        };
        SessionBackend::resume(&m, "T1", base, &delta)
            .await
            .unwrap();
        let log = std::fs::read_to_string(&log).unwrap();
        assert_eq!(log.matches("--start--").count(), 1, "one process: {log}");
        assert!(log.contains("app-server\n-c\nsandbox_mode"), "{log}");
        assert!(log.contains(r#""approvalPolicy":"never""#), "{log}");
        assert!(
            log.contains(r#""baseInstructions":"#),
            "the protocol is the thread's instructions"
        );
        assert!(log.contains(r#""outputSchema":"#));
        assert!(
            log.contains(r#""id":"r1""#) && log.contains("-32601"),
            "Codex's own request is declined: {log}"
        );
        assert_eq!(log.matches(r#""method":"turn/start""#).count(), 2);
        assert!(
            !log.contains("thread/resume"),
            "the process holds the thread"
        );

        // After a discard the process is gone; the thread is loaded again before its turn.
        SessionBackend::discard(&m, "T1").await;
        SessionBackend::resume(&m, "T1", base, &delta)
            .await
            .unwrap();
        let log = std::fs::read_to_string(m.program().unwrap().with_file_name("log")).unwrap();
        assert_eq!(log.matches("--start--").count(), 2);
        assert!(log.contains("thread/resume"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_model_that_gave_up_after_refused_native_calls_is_asked_once_more() {
        // The first run tries a native call, is refused and gives up; the next calls the tool.
        let refused = json!({"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","name":"fs.read","input":{}}}});
        let gave_up = format!(
            "{refused}\n{}",
            stream_reply(reply("I cannot access the tools."), usage("S1", 6, 900))
        );
        let called = stream_reply(
            json!({"action":"call_tools","text":"","tool_calls":[{"name":"fs.read","arguments":"{}"}]}),
            usage("S1", 2, 300),
        );
        let (bin, log) = claude_stream("claude-gave-up", &[gave_up.clone(), called]);
        let m = CliModel::new(Cli::Claude, "haiku").with_program(bin.clone());
        let shown = Mutex::new(String::new());
        let sink = |t: &str| shown.lock().unwrap().push_str(t);
        let c = m
            .complete_streaming("sys", &[Message::user("read a")], &tools(), &sink)
            .await
            .unwrap();
        assert_eq!(c.tool_calls.len(), 1, "asked again, it called the tool");
        assert_eq!(c.usage.prompt_tokens, 1200, "both runs are counted");
        assert_eq!(
            *shown.lock().unwrap(),
            "",
            "the run that gave up showed nothing"
        );
        let log = std::fs::read_to_string(&log).unwrap();
        assert_eq!(log.matches("--line--").count(), 2);
        assert!(log.contains("gave up because calling host tools directly failed"));

        // Without tools a plain reply is just the answer: no second run.
        let (bin, _) = claude_stream("claude-gave-up-2", &[gave_up]);
        let m = CliModel::new(Cli::Claude, "haiku").with_program(bin);
        let c = m
            .complete("sys", &[Message::user("hi")], &[])
            .await
            .unwrap();
        assert!(c.tool_calls.is_empty());
        assert_eq!(c.usage.prompt_tokens, 900);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_reply_after_structured_output_enforcement_is_not_asked_again() {
        // Claude Code adds turns when it makes the model resend a plain-text answer through
        // StructuredOutput. No native call was refused, so the answer stands.
        let (bin, log) = claude_stream(
            "claude-enforced",
            &[stream_reply(reply("6.5 seconds."), usage("S1", 3, 900))],
        );
        let m = CliModel::new(Cli::Claude, "sonnet").with_program(bin);
        let shown = Mutex::new(String::new());
        let sink = |t: &str| shown.lock().unwrap().push_str(t);
        let c = m
            .complete_streaming("sys", &[Message::user("how long?")], &tools(), &sink)
            .await
            .unwrap();
        assert_eq!(c.content, "6.5 seconds.");
        assert_eq!(c.usage.prompt_tokens, 900, "one run");
        assert_eq!(*shown.lock().unwrap(), "6.5 seconds.", "shown once");
        let log = std::fs::read_to_string(&log).unwrap();
        assert_eq!(log.matches("--line--").count(), 1, "{log}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn claude_text_streams_while_the_reply_is_written() {
        let answer = "Line one.\nLine \"two\".";
        let (bin, log) = claude_stream(
            "claude-stream",
            &[stream_reply(reply(answer), usage("S1", 2, 10))],
        );
        let m = CliModel::new(Cli::Claude, "haiku").with_program(bin);
        let pieces = Mutex::new(Vec::<String>::new());
        let sink = |t: &str| pieces.lock().unwrap().push(t.to_string());
        let c = m
            .complete_streaming("sys", &[Message::user("hi")], &[], &sink)
            .await
            .unwrap();
        assert_eq!(c.content, answer);
        let pieces = pieces.into_inner().unwrap();
        assert!(pieces.len() > 1, "{pieces:?}");
        assert_eq!(pieces.concat(), answer);
        let log = std::fs::read_to_string(log).unwrap();
        assert!(
            log.contains("--no-session-persistence"),
            "a one-shot call keeps nothing"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_claude_process_that_stops_mid_reply_is_an_error_with_its_last_words() {
        let bin = fake_raw(
            "claude-dies",
            "read l; echo '{\"type\":\"system\",\"subtype\":\"init\"}'; echo 'API Error: 529 overloaded' >&2; sleep 0.2; exit 1",
        );
        let m = CliModel::new(Cli::Claude, "").with_program(bin);
        let e = m
            .complete("", &[Message::user("hi")], &[])
            .await
            .unwrap_err();
        assert!(e.to_string().contains("529 overloaded"), "{e}");
        assert!(e.is_transient(), "an overloaded account is retried later");
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
    fn overload_is_retried_and_a_usage_limit_says_when_it_resets() {
        let m = CliModel::new(Cli::Claude, "");
        assert!(m
            .classify("API Error: 529 overloaded".into())
            .is_transient());
        assert!(!m.classify("bad flag".into()).is_transient());
        let in_two_hours = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 2 * 3600
            + 600;
        let e = m.classify(format!("Claude AI usage limit reached|{in_two_hours}"));
        assert!(!e.is_transient(), "hours away: not retried inside a turn");
        let text = e.to_string();
        assert!(
            text.contains("Claude subscription: Claude AI usage limit reached"),
            "{text}"
        );
        assert!(text.contains("resets in 2 h 10 min"), "{text}");
        assert!(text.contains("/model"), "{text}");
        let e = m.classify_with("You've hit your usage limit.".into(), Some(in_two_hours));
        assert!(e.to_string().contains("resets in 2 h"), "{e}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bad_arguments_are_malformed_and_retryable() {
        let out = stream_reply(
            json!({"action":"call_tools","text":"","tool_calls":[{"name":"fs.read","arguments":"{path"}]}),
            json!({"is_error":false,"usage":{}}),
        );
        let (bin, _) = claude_stream("claude-bad", &[out]);
        let m = CliModel::new(Cli::Claude, "").with_program(bin);
        let e = m
            .complete("", &[Message::user("hi")], &tools())
            .await
            .unwrap_err();
        assert!(matches!(e, ProviderError::Malformed { .. }));
        assert!(e.is_transient());
    }

    #[test]
    fn claude_arguments_are_an_object_and_codex_arguments_are_typed() {
        let m = CliModel::new(Cli::Claude, "");
        let reply = json!({"action":"call_tools","text":"","tool_calls":[
            {"name":"code.run","arguments":{"code":"print(\"a\\nb\")"}},
            {"name":"fs.read","arguments":"{\"path\": \"a\"}"}
        ]});
        let c = m
            .completion(&reply, Usage::default(), &ReplyShape::open())
            .unwrap();
        assert_eq!(
            c.tool_calls[0].arguments,
            json!({"code":"print(\"a\\nb\")"})
        );
        assert_eq!(c.tool_calls[1].arguments, json!({"path":"a"}));
        let schema: Value = serde_json::from_str(CLAUDE_REPLY_SCHEMA).unwrap();
        assert_eq!(
            schema["properties"]["tool_calls"]["items"]["properties"]["arguments"]["type"],
            "object"
        );
        assert!(protocol(Some(Cli::Claude), "", &[], false).contains("arguments as a JSON object)"));
        assert!(protocol(Some(Cli::Codex), "", &[], false)
            .contains("arguments as a JSON object, as the reply schema shapes it"));
        let schema = codex_reply_schema(&tools());
        assert_eq!(strict_problems(&schema), Vec::<String>::new());
        let call = &schema["properties"]["tool_calls"]["items"]["properties"];
        assert_eq!(call["name"]["enum"], json!(["fs.read"]));
        assert_eq!(
            call["arguments"]["properties"]["path"],
            json!({"anyOf":[{"type":"string"},{"type":"null"}]})
        );
        assert_eq!(
            codex_reply_schema(&[]).to_string(),
            json!(serde_json::from_str::<Value>(REPLY_SCHEMA).unwrap()).to_string()
        );
    }

    #[test]
    fn subscription_specs_keep_model_and_effort_separate() {
        remember_codex_models(&[crate::ModelOption {
            id: "gpt-5.6-terra".into(),
            label: "GPT-5.6-Terra".into(),
            efforts: vec!["low".into(), "medium".into()],
            default_effort: Some("medium".into()),
        }]);
        let m = CliModel::try_new(Cli::Codex, "gpt-5.6-terra@medium").unwrap();
        assert_eq!(m.model, "gpt-5.6-terra");
        assert_eq!(m.effort.as_deref(), Some("medium"));
        assert_eq!(Provider::model_id(&m), "codex:gpt-5.6-terra@medium");
        let args = claude_stream_args(
            Mode::OneShot,
            "sonnet",
            Some("high"),
            None,
            CLAUDE_REPLY_SCHEMA,
        );
        assert!(args.windows(2).any(|a| a == ["--effort", "high"]));
        assert!(CliModel::try_new(Cli::Claude, "sonnet@fast").is_err());
        let direct = CliModel::new(Cli::Claude, "sonnet@fast");
        assert_eq!(direct.model, "sonnet");
        assert_eq!(direct.effort.as_deref(), Some("fast"));
        assert!(CliModel::try_new(Cli::Codex, "gpt-5.6-terra@ultra").is_err());
        assert!(CliModel::try_new(Cli::Codex, "not-advertised@medium").is_err());
    }

    #[test]
    fn an_error_without_words_names_its_kind() {
        let e = parse_claude(r#"{"is_error":true,"subtype":"error_max_turns","num_turns":9}"#);
        assert_eq!(
            e.err().as_deref(),
            Some("the CLI reported an error (error_max_turns)")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn signed_out_error_says_how_to_sign_in() {
        let out =
            r#"{"type":"result","is_error":true,"result":"Not logged in · Please run /login"}"#;
        let m = CliModel::new(Cli::Claude, "").with_program(fake_raw(
            "claude-out",
            &format!("read l; echo '{out}'; exit 1"),
        ));
        let e = m
            .complete("", &[Message::user("hi")], &[])
            .await
            .unwrap_err();
        assert!(e.to_string().contains("claude auth login"), "{e}");
    }
    /// `files.edit` as ark-files describes it (ADR 0049): each edit a closed variant.
    fn edit_tool() -> ToolDef {
        ToolDef {
            name: "files.edit".into(),
            description: "Change part of a file".into(),
            parameters: json!({
                "type": "object", "required": ["path", "edits"],
                "properties": {
                    "path": { "type": "string" },
                    "edits": { "type": "array", "items": { "anyOf": [
                        { "type": "object", "required": ["find", "replace"], "properties": { "find": { "type": "string" }, "replace": { "type": "string" } } },
                        { "type": "object", "required": ["block", "text"], "properties": { "block": { "type": "integer" }, "text": { "type": "string" } } }
                    ] } },
                    "version": { "type": "string" }
                }
            }),
        }
    }

    /// JavaScript like the 6,043-character edit Codex once sent as a string with one quote
    /// left unescaped: quotes, backslashes, newlines, tabs, braces, non-ASCII.
    fn long_code() -> String {
        "const sep = (area.value.endsWith(\"\\n\") ? \" \" : \"\");\n\tif (a) { b = {x: [1, \"two\"]}; }\n"
            .repeat(70)
            + "console.log('done ✓ é', \"\\\"quoted\\\"\");"
    }

    #[tokio::test]
    async fn codex_app_turns_get_the_tools_schema_and_long_calls_arrive_whole() {
        let code = long_code();
        let answer = json!({"action":"call_tools","text":"Editing the file.","tool_calls":[
            {"name":"files.edit","arguments":{"path":"src/app.js","edits":[
                {"find":"const a = \"x\";","replace": code},
                {"block":2,"text":"# Title\n- a\n- b"}
            ],"version":null}},
            {"name":"fs.read","arguments":{"path":"src/app.js"}}
        ]})
        .to_string();
        let (bin, log) = codex_app("codex-long", &answer);
        let m = CliModel::new(Cli::Codex, "gpt-5.6-terra@medium").with_program(bin);
        let shown = Mutex::new(String::new());
        let sink = |t: &str| shown.lock().unwrap().push_str(t);
        let tools = vec![edit_tool(), tools().remove(0)];
        let (_, c) =
            SessionBackend::open_streaming(&m, "", &tools, &[Message::user("edit it")], &sink)
                .await
                .unwrap();
        assert_eq!(c.tool_calls.len(), 2, "several tools in one reply");
        assert_eq!(
            c.tool_calls[0].arguments,
            json!({"path":"src/app.js","edits":[
                {"find":"const a = \"x\";","replace": code},
                {"block":2,"text":"# Title\n- a\n- b"}
            ]}),
            "the arguments reach the tool exactly; a null optional key is left out"
        );
        assert_eq!(c.tool_calls[1].arguments, json!({"path":"src/app.js"}));
        assert_eq!(
            shown.into_inner().unwrap(),
            "Editing the file.",
            "only the reply's own text is shown, not a `text` key in the arguments"
        );
        let log = std::fs::read_to_string(&log).unwrap();
        let turn = log
            .lines()
            .find(|l| l.contains(r#""method":"turn/start""#))
            .unwrap();
        let sent: Value = serde_json::from_str(turn).unwrap();
        assert_eq!(sent["params"]["effort"], "medium");
        let schema = &sent["params"]["outputSchema"];
        assert_eq!(strict_problems(schema), Vec::<String>::new(), "{schema}");
        let names: Vec<&Value> = schema["properties"]["tool_calls"]["items"]["anyOf"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| &v["properties"]["name"]["enum"][0])
            .collect();
        assert_eq!(names, [&json!("files.edit"), &json!("fs.read")]);
    }

    #[tokio::test]
    async fn a_resumed_codex_session_gets_the_schema_of_the_tools_available_now() {
        let (bin, log) = codex_app(
            "codex-tools-now",
            r#"{"action":"reply","text":"ok","tool_calls":[]}"#,
        );
        let m = CliModel::new(Cli::Codex, "").with_program(bin);
        let first = tools();
        let (vendor, _) = SessionBackend::open(&m, "", &first, &[Message::user("hi")])
            .await
            .unwrap();
        let delta = Delta {
            messages: vec![Message::user("now edit")],
            new_tools: vec![edit_tool()],
            tools_now: Some(vec!["fs.read".into(), "files.edit".into()]),
            tools: vec![tools().remove(0), edit_tool()],
            ..Default::default()
        };
        let base = Base {
            system: "",
            tools: &first,
        };
        SessionBackend::resume(&m, &vendor, base, &delta)
            .await
            .unwrap();
        let log = std::fs::read_to_string(&log).unwrap();
        let turns: Vec<&str> = log
            .lines()
            .filter(|l| l.contains(r#""method":"turn/start""#))
            .collect();
        assert_eq!(turns.len(), 2);
        assert!(!turns[0].contains(r#""enum":["files.edit"]"#));
        assert!(
            turns[1].contains(r#""enum":["files.edit"]"#),
            "{}",
            turns[1]
        );
    }

    #[tokio::test]
    async fn host_tools_are_disabled_for_both_started_and_reloaded_threads() {
        let (bin, log) = codex_app(
            "host-isolation",
            r#"{"action":"reply","text":"ok","tool_calls":[]}"#,
        );
        let mut app = crate::codex_app::CodexApp::start(&bin, "test")
            .await
            .unwrap();
        app.start_thread(bin.parent().unwrap(), None, "host")
            .await
            .unwrap();
        app.resume_thread(bin.parent().unwrap(), None, "other-thread")
            .await
            .unwrap();
        app.stop().await;
        let log = std::fs::read_to_string(log).unwrap();
        for method in ["thread/start", "thread/resume"] {
            let line = log
                .lines()
                .find(|l| l.contains(&format!("\"method\":\"{method}\"")))
                .unwrap();
            let sent: Value = serde_json::from_str(line).unwrap();
            assert_eq!(
                sent["params"]["config"]["mcp_servers.unused-server.enabled"],
                false
            );
            assert_eq!(sent["params"]["config"]["features.shell_tool"], false);
            assert_eq!(sent["params"]["config"]["features.apps"], false);
            assert_eq!(sent["params"]["config"]["web_search"], "disabled");
        }
    }

    #[test]
    fn host_exec_flags_preserve_only_explicit_web_search() {
        let mut normal = command(Path::new("codex"));
        host_codex_flags(&mut normal, false);
        let args: Vec<_> = normal.as_std().get_args().collect();
        assert!(args.contains(&std::ffi::OsStr::new("features.shell_tool=false")));
        assert!(args.contains(&std::ffi::OsStr::new("features.apps=false")));
        assert!(args.contains(&std::ffi::OsStr::new("web_search=\"disabled\"")));
        let mut search = command(Path::new("codex"));
        host_codex_flags(&mut search, true);
        assert!(!search
            .as_std()
            .get_args()
            .any(|v| v == "web_search=\"disabled\""));
    }

    #[tokio::test]
    async fn model_discovery_does_not_require_a_host_compatible_mcp_name() {
        let (bin, log) = codex_app(
            "catalog-with-dotted-server",
            r#"{"action":"reply","text":"ok","tool_calls":[]}"#,
        );
        let script = std::fs::read_to_string(&bin)
            .unwrap()
            .replace("unused-server", "name.with.dot");
        std::fs::write(&bin, script).unwrap();
        let mut app = crate::codex_app::CodexApp::start(&bin, "test")
            .await
            .unwrap();
        let models = app.models().await.unwrap();
        remember_codex_models(&models);
        assert!(CliModel::try_new(Cli::Codex, "gpt-5.6-terra@medium").is_ok());
        assert!(app
            .start_thread(bin.parent().unwrap(), None, "host")
            .await
            .is_err());
        app.stop().await;
        let log = std::fs::read_to_string(log).unwrap();
        assert!(log.find("model/list").unwrap() < log.find("config/read").unwrap());
        assert!(!log.contains("\"method\":\"thread/start\""));
    }

    #[tokio::test]
    async fn incompatible_mcp_names_fall_back_before_submitting_a_vendor_turn() {
        let (bin, log) = codex_app(
            "isolated-exec-fallback",
            r#"{"action":"reply","text":"unused","tool_calls":[]}"#,
        );
        let answer = json!({"action":"reply","text":"fallback answer","tool_calls":[]}).to_string();
        let events = format!(
            "{}\n{}\n{}",
            json!({"type":"thread.started","thread_id":"fallback-thread"}),
            json!({"type":"item.completed","item":{"type":"agent_message","text":answer}}),
            json!({"type":"turn.completed","usage":{"input_tokens":50,"output_tokens":4}})
        );
        let script = std::fs::read_to_string(&bin)
            .unwrap()
            .replace("unused-server", "name.with.dot");
        let branch = format!("#!/bin/sh\nif [ \"$1\" != app-server ]; then\ncat > /dev/null\ncat <<'ARK_FAKE_REPLY'\n{events}\nARK_FAKE_REPLY\nexit 0\nfi\n");
        std::fs::write(&bin, script.replacen("#!/bin/sh\n", &branch, 1)).unwrap();
        let model = CliModel::new(Cli::Codex, "").with_program(&bin);
        let (_, completion) = SessionBackend::open(&model, "host", &[], &[Message::user("hello")])
            .await
            .unwrap();
        assert_eq!(completion.content, "fallback answer");
        assert!(model.no_app.load(Ordering::Relaxed));
        let log = std::fs::read_to_string(log).unwrap();
        assert!(!log.contains("\"method\":\"thread/start\""));
        assert!(!log.contains("\"method\":\"turn/start\""));
    }

    /// A fake `codex exec` that keeps each reply schema it is given (`schema-<n>.json` next to
    /// it) and answers with `reply`. With `refuse`, a typed schema is refused as OpenAI refuses
    /// one it cannot check.
    fn codex_exec(name: &str, reply: &Value, refuse: bool) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("ark-codex-exec-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let events = [
            json!({"type":"item.completed","item":{"id":"i","type":"agent_message","text": reply.to_string()}}),
            json!({"type":"turn.completed","usage":{"input_tokens":50,"output_tokens":4}}),
        ];
        let out: String = events.iter().map(|e| format!("{e}\n")).collect();
        std::fs::write(dir.join("reply"), out).unwrap();
        let refusal = r#"{"type":"turn.failed","error":{"message":"Invalid schema for response_format codex_output_schema: ... (invalid_json_schema)"}}"#;
        let p = dir.join(name);
        std::fs::write(
            &p,
            format!(
                r#"#!/bin/sh
[ "$1" = app-server ] && exit 1
d=$(dirname "$0")
cat > /dev/null
prev=""
for a in "$@"; do
  if [ "$prev" = "--output-schema" ]; then
    n=$(ls "$d" | grep -c '^schema-')
    cp "$a" "$d/schema-$n.json"
    s="$a"
  fi
  prev="$a"
done
if [ {refuse} = 1 ] && grep -q '"arguments":{{"type":"object"' "$s"; then
  echo '{refusal}'
  exit 1
fi
cat "$d/reply"
"#,
                refuse = u8::from(refuse)
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        p
    }

    #[tokio::test]
    async fn codex_exec_gets_the_tools_schema_and_returns_objects() {
        let reply = json!({"action":"call_tools","text":"","tool_calls":[
            {"name":"files.edit","arguments":{"path":"a.js","edits":[{"find":"x","replace": long_code()}],"version":"v1"}}
        ]});
        let bin = codex_exec("codex", &reply, false);
        let m = CliModel::new(Cli::Codex, "").with_program(&bin);
        let c = m
            .complete("", &[Message::user("edit")], &[edit_tool()])
            .await
            .unwrap();
        assert_eq!(
            c.tool_calls[0].arguments,
            json!({"path":"a.js","edits":[{"find":"x","replace": long_code()}],"version":"v1"})
        );
        let schema: Value = serde_json::from_str(
            &std::fs::read_to_string(bin.with_file_name("schema-0.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(strict_problems(&schema), Vec::<String>::new());
        assert_eq!(
            schema["properties"]["tool_calls"]["items"]["properties"]["name"]["enum"],
            json!(["files.edit"])
        );
    }

    #[tokio::test]
    async fn typed_arguments_that_are_not_json_where_json_is_due_are_malformed() {
        let open = ToolDef {
            name: "mcp.put".into(),
            description: String::new(),
            parameters: json!({"type":"object","required":["data"],"properties":{"data":{"type":"object"}}}),
        };
        let reply = json!({"action":"call_tools","text":"","tool_calls":[
            {"name":"mcp.put","arguments":{"data":"{\"a\": \"cut off"}}
        ]});
        let m = CliModel::new(Cli::Codex, "").with_program(codex_exec("codex-bad", &reply, false));
        let e = m
            .complete("", &[Message::user("put")], &[open])
            .await
            .unwrap_err();
        assert!(matches!(e, ProviderError::Malformed { .. }), "{e}");
        assert!(e.is_transient(), "a new reply may be well formed");
        assert!(
            e.to_string()
                .contains("arguments for mcp.put: `data` is not JSON"),
            "{e}"
        );
    }

    #[tokio::test]
    async fn a_refused_schema_switches_the_model_to_strings_and_the_call_is_retryable() {
        // The reply is in the older form, as Codex writes it under REPLY_SCHEMA.
        let reply = json!({"action":"call_tools","text":"","tool_calls":[
            {"name":"fs.read","arguments":"{\"path\": \"a\"}"}
        ]});
        let bin = codex_exec("codex-refuse", &reply, true);
        let m = CliModel::new(Cli::Codex, "refusing-model").with_program(&bin);
        let e = m
            .complete("", &[Message::user("read a")], &tools())
            .await
            .unwrap_err();
        assert!(matches!(e, ProviderError::Refused { .. }), "{e}");
        assert!(e.is_transient());
        // The retry, and every later call of this model, sends the string form.
        let c = m
            .complete("", &[Message::user("read a")], &tools())
            .await
            .unwrap();
        assert_eq!(c.tool_calls[0].arguments, json!({"path":"a"}));
        let second = std::fs::read_to_string(bin.with_file_name("schema-1.json")).unwrap();
        assert!(
            second.contains(r#""arguments":{"type":"string"}"#),
            "{second}"
        );
        let other = CliModel::new(Cli::Codex, "refusing-model").with_program(&bin);
        other
            .complete("", &[Message::user("read a")], &tools())
            .await
            .unwrap();
        let third = std::fs::read_to_string(bin.with_file_name("schema-2.json")).unwrap();
        assert!(
            third.contains(r#""arguments":{"type":"string"}"#),
            "another lane of the same model does not pay the refusal again"
        );
    }
}
