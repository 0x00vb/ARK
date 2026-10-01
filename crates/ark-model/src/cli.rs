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
//! Each call starts a process and resends the whole transcript, so it is slower than an API.
//! No streaming: text arrives when the reply is complete.

use crate::{Completion, Message, Provider, ProviderError, Result, Role, ToolCall, ToolDef, Usage};
use async_trait::async_trait;
use serde::Serialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
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
        let bin = self.program()?;
        let sys = protocol_prompt(system, tools);
        let transcript = render_transcript(messages);
        let dir = work_dir()?;
        let (mut cmd, stdin, out_file) = match self.cli {
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
                .args(["--setting-sources", "", "--no-session-persistence"])
                .args(["--model", &self.model, "--system-prompt", &sys])
                .args(["--json-schema", REPLY_SCHEMA]);
                (c, transcript, None)
            }
            Cli::Codex => {
                let n = self.calls.fetch_add(1, Ordering::Relaxed);
                let tag = format!("{}-{n}", std::process::id());
                let schema = dir.join(format!("schema-{tag}.json"));
                std::fs::write(&schema, REPLY_SCHEMA).map_err(io_err)?;
                let mut c = command(&bin);
                c.args(["exec", "--json", "--skip-git-repo-check", "--ephemeral"])
                    .args(["--ignore-rules", "--ignore-user-config", "-s", "read-only"])
                    .arg("-C")
                    .arg(&dir)
                    .arg("--output-schema")
                    .arg(&schema);
                if self.model != "default" {
                    c.args(["-m", &self.model]);
                }
                c.arg("-");
                (c, format!("{sys}\n\n{transcript}"), Some(schema))
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
                w.write_all(stdin.as_bytes()).await.map_err(io_err)?;
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
            Ok((reply, usage)) => self.completion(&reply, usage),
            Err(detail) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                let tail = stderr.trim().lines().last().unwrap_or("");
                let detail = if out.status.success() || tail.is_empty() {
                    detail
                } else {
                    format!("{detail} ({tail})")
                };
                Err(self.api(self.signed_out_hint(detail)))
            }
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
    let mut s = String::from(
        "You are the model behind a host application's agent. The host owns the tools listed \
below and runs them. You have no tools of your own and must not try to run anything yourself.\n\n\
Answer every turn with exactly one action:\n\
- \"call_tools\": put one or more calls in tool_calls (name, and arguments as a JSON object \
string). text may hold a short note. The host runs the calls and sends the results back.\n\
- \"reply\": your answer to the person goes in text, in Markdown. tool_calls must be empty.\n\n\
Earlier results appear in the transcript as <tool_result>. Never repeat a call whose result \
you already have.\n",
    );
    if tools.is_empty() {
        s.push_str("\nNo host tools are available in this turn. Use \"reply\".\n");
    } else {
        s.push_str("\n# Host tools\n");
        for t in tools {
            s.push_str(&format!(
                "\n## {}\n{}\nParameters (JSON Schema): {}\n",
                t.name,
                t.description.trim(),
                t.parameters
            ));
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
    s.push_str("</transcript>\nContinue as the assistant with one action.\n");
    s
}

/// `claude -p --output-format json`: one result object.
fn parse_claude(stdout: &str) -> std::result::Result<(Value, Usage), String> {
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
    Ok((reply, usage))
}

/// `codex exec --json`: JSONL events; the last agent message is the reply.
fn parse_codex(stdout: &str) -> std::result::Result<(Value, Usage), String> {
    let mut reply = None;
    let mut usage = Usage::default();
    let mut failure = None;
    for line in stdout.lines() {
        let Ok(ev) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match ev["type"].as_str() {
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
    Ok((v, usage))
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
