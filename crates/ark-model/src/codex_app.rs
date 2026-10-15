//! Codex through `codex app-server`: JSON-RPC over stdio, one process holding the lane's
//! threads. A thread is a vendor session (`thread/start`, `thread/resume`); a call is a turn
//! (`turn/start` with ARK's reply schema), whose reply arrives as text deltas. On a running
//! process a short turn took 1.7 s where `codex exec` took 23 s (measured 2026-10-04, Codex
//! 0.153). The app server is marked experimental by Codex; when it cannot be started, the
//! caller falls back to `codex exec`.

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::live::{LiveProcess, ReadError};
use crate::stream::JsonTextField;
use crate::{ModelOption, TextSink, Usage};

/// How long the handshake and a thread start may take. A thread start also starts the MCP
/// servers of the person's Codex setup.
const SETUP_TIMEOUT: Duration = Duration::from_secs(60);

pub(crate) struct CodexApp {
    proc: LiveProcess,
    next_id: u64,
    /// Threads this process has started or loaded.
    threads: std::collections::HashSet<String>,
    host_config: Option<Value>,
}

/// One turn's outcome.
pub(crate) struct Turn {
    /// The agent message: ARK's structured reply as JSON text.
    pub text: String,
    pub usage: Usage,
}

pub(crate) struct TurnRequest<'a> {
    pub thread_id: &'a str,
    pub input: &'a str,
    pub pictures: &'a [crate::Picture],
    pub schema: &'a Value,
    pub effort: Option<&'a str>,
    pub wait: Duration,
    pub on_text: Option<TextSink<'a>>,
}

/// Why a request or turn did not complete.
#[derive(Debug)]
pub(crate) enum AppError {
    /// The process or protocol failed: the app is not usable any more.
    Broken(String),
    /// Codex answered with an error (a failed turn, a refused request). The app still works.
    Failed(String),
    TimedOut,
}

impl CodexApp {
    /// Ordinary sessions collect account usage before their first turn.
    pub(crate) async fn start(bin: &Path, version: &str) -> Result<Self, AppError> {
        let mut app = Self::start_catalog(bin, version).await?;
        // The account's usage limits, before any turn. A sign-in without them (an API key)
        // answers with an error, which changes nothing.
        if let Ok(r) = app
            .request("account/rateLimits/read", Value::Null, SETUP_TIMEOUT)
            .await
        {
            record_limits(&r["rateLimits"]);
        }
        Ok(app)
    }

    /// Start only the process and handshake. Catalog discovery needs no usage query.
    pub(crate) async fn start_catalog(bin: &Path, version: &str) -> Result<Self, AppError> {
        let mut cmd = crate::cli::command(bin);
        cmd.args(["app-server", "-c", "sandbox_mode=\"read-only\""]);
        crate::cli::host_codex_flags(&mut cmd, false);
        let proc = LiveProcess::spawn(cmd)
            .map_err(|e| AppError::Broken(format!("could not start: {e}")))?;
        let mut app = CodexApp {
            proc,
            next_id: 0,
            threads: Default::default(),
            host_config: None,
        };
        app.request(
            "initialize",
            json!({"clientInfo": {"name": "ark", "version": version}}),
            SETUP_TIMEOUT,
        )
        .await?;
        app.notify("initialized").await?;
        Ok(app)
    }

    pub(crate) fn running(&mut self) -> bool {
        self.proc.running()
    }

    pub(crate) async fn stop(self) {
        self.proc.stop().await;
    }

    /// Catalog discovery stays independent of host-model isolation and its exec fallback.
    pub(crate) async fn prepare_host(&mut self) -> Result<Value, AppError> {
        if let Some(config) = &self.host_config {
            return Ok(config.clone());
        }
        // Only names leave this response. Never log or persist its credential values.
        let response = self
            .request(
                "config/read",
                json!({"includeLayers": false}),
                SETUP_TIMEOUT,
            )
            .await?;
        let config = host_config(&response)?;
        self.host_config = Some(config.clone());
        Ok(config)
    }

    /// Models and effort levels available to this signed-in Codex account.
    pub(crate) async fn models(&mut self) -> Result<Vec<ModelOption>, AppError> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..20 {
            let r = self
                .request(
                    "model/list",
                    json!({"cursor": cursor, "includeHidden": false}),
                    SETUP_TIMEOUT,
                )
                .await?;
            out.extend(r["data"].as_array().into_iter().flatten().filter_map(|m| {
                if m["hidden"].as_bool() == Some(true) {
                    return None;
                }
                let id = m["model"].as_str().or_else(|| m["id"].as_str())?;
                Some(ModelOption {
                    id: id.to_string(),
                    label: m["displayName"].as_str().unwrap_or(id).to_string(),
                    efforts: m["supportedReasoningEfforts"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|e| e["reasoningEffort"].as_str().map(str::to_string))
                        .collect(),
                    default_effort: m["defaultReasoningEffort"].as_str().map(str::to_string),
                })
            }));
            let next = r["nextCursor"].as_str().map(str::to_string);
            if next.is_none() || next == cursor {
                break;
            }
            cursor = next;
        }
        Ok(out)
    }

    /// Start a thread: no approvals, read-only, `instructions` as its base instructions.
    pub(crate) async fn start_thread(
        &mut self,
        cwd: &Path,
        model: Option<&str>,
        instructions: &str,
    ) -> Result<String, AppError> {
        let mut p = thread_settings(cwd, model);
        p["config"] = self.prepare_host().await?;
        p["baseInstructions"] = json!(instructions);
        p["ephemeral"] = json!(false);
        let r = self.request("thread/start", p, SETUP_TIMEOUT).await?;
        let id = r["thread"]["id"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| AppError::Broken("thread/start returned no thread id".into()))?;
        self.threads.insert(id.clone());
        Ok(id)
    }

    /// Whether this process holds thread `id`.
    pub(crate) fn has_thread(&self, id: &str) -> bool {
        self.threads.contains(id)
    }

    /// Load a thread this process has not started (after a restart).
    pub(crate) async fn resume_thread(
        &mut self,
        cwd: &Path,
        model: Option<&str>,
        id: &str,
    ) -> Result<(), AppError> {
        let mut p = thread_settings(cwd, model);
        p["config"] = self.prepare_host().await?;
        p["threadId"] = json!(id);
        self.request("thread/resume", p, SETUP_TIMEOUT).await?;
        self.threads.insert(id.to_string());
        Ok(())
    }

    /// Run one turn on thread `id`, its reply shaped by `schema`. The reply's `text` field
    /// reaches `on_text` while it is written.
    pub(crate) async fn turn(&mut self, request: TurnRequest<'_>) -> Result<Turn, AppError> {
        let TurnRequest {
            thread_id: id,
            input,
            pictures,
            schema,
            effort,
            wait,
            on_text,
        } = request;
        let deadline = Instant::now() + wait;
        let mut params = json!({
            "threadId": id,
            "input": std::iter::once(json!({"type": "text", "text": input}))
                .chain(pictures.iter().map(|p| json!({"type": "image", "url": p.data_url()})))
                .collect::<Vec<_>>(),
            "outputSchema": schema,
        });
        if let Some(effort) = effort {
            params["effort"] = json!(effort);
        }
        let rid = self.send_request("turn/start", params).await?;
        let mut field = JsonTextField::new("text");
        let mut text: Option<String> = None;
        let mut usage = Usage::default();
        loop {
            let m = self.next(deadline).await?;
            if m["id"].as_u64() == Some(rid) {
                if let Some(e) = m.get("error") {
                    return Err(AppError::Failed(error_text(e)));
                }
                continue;
            }
            let params = &m["params"];
            if params["threadId"].as_str().is_some_and(|t| t != id) {
                continue;
            }
            match m["method"].as_str() {
                Some("item/agentMessage/delta") => {
                    if let Some(f) = on_text {
                        let t = field.push(params["delta"].as_str().unwrap_or(""));
                        if !t.is_empty() {
                            f(&t);
                        }
                    }
                }
                Some("item/completed") if params["item"]["type"] == "agentMessage" => {
                    text = params["item"]["text"].as_str().map(str::to_string);
                }
                Some("account/rateLimits/updated") => record_limits(&params["rateLimits"]),
                Some("thread/tokenUsage/updated") => {
                    usage = turn_usage(&params["tokenUsage"]["last"]);
                }
                Some("error") => {
                    let detail = error_text(&params["error"]);
                    if params["willRetry"].as_bool() != Some(true) {
                        return Err(AppError::Failed(detail));
                    }
                }
                Some("turn/completed") => {
                    let turn = &params["turn"];
                    return match turn["status"].as_str() {
                        Some("completed") => Ok(Turn {
                            text: text.ok_or_else(|| {
                                AppError::Failed("the turn ended with no reply".into())
                            })?,
                            usage,
                        }),
                        _ => Err(AppError::Failed(
                            turn["error"]
                                .get("message")
                                .and_then(Value::as_str)
                                .unwrap_or("the turn did not complete")
                                .to_string(),
                        )),
                    };
                }
                _ => {}
            }
        }
    }

    /// The last line Codex wrote to stderr.
    pub(crate) fn stderr_tail(&self) -> String {
        self.proc.stderr_tail()
    }

    async fn request(
        &mut self,
        method: &str,
        params: Value,
        wait: Duration,
    ) -> Result<Value, AppError> {
        let deadline = Instant::now() + wait;
        let id = self.send_request(method, params).await?;
        loop {
            let m = self.next(deadline).await?;
            if m["id"].as_u64() == Some(id) && m.get("method").is_none() {
                if let Some(e) = m.get("error") {
                    return Err(AppError::Failed(format!("{method}: {}", error_text(e))));
                }
                return Ok(m["result"].clone());
            }
        }
    }

    async fn send_request(&mut self, method: &str, params: Value) -> Result<u64, AppError> {
        self.next_id += 1;
        let id = self.next_id;
        let m = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        self.write(&m).await?;
        Ok(id)
    }

    async fn notify(&mut self, method: &str) -> Result<(), AppError> {
        self.write(&json!({"jsonrpc": "2.0", "method": method}))
            .await
    }

    async fn write(&mut self, m: &Value) -> Result<(), AppError> {
        self.proc
            .send(&m.to_string())
            .await
            .map_err(|e| AppError::Broken(e.to_string()))
    }

    /// The next message. A request from Codex (an approval, a question for the person) is
    /// declined at once: ARK's tools run in ARK, and Codex's own are not offered.
    async fn next(&mut self, deadline: Instant) -> Result<Value, AppError> {
        loop {
            let wait = deadline.saturating_duration_since(Instant::now());
            let line = match self.proc.next_line(wait).await {
                Ok(l) => l,
                Err(ReadError::TimedOut) => return Err(AppError::TimedOut),
                Err(ReadError::Closed) => return Err(AppError::Broken("it stopped".into())),
                Err(ReadError::Io(e)) => return Err(AppError::Broken(e.to_string())),
            };
            let Ok(m) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if m.get("method").is_some() && m.get("id").is_some_and(|i| !i.is_null()) {
                let no = json!({"jsonrpc": "2.0", "id": m["id"], "error": {
                    "code": -32601,
                    "message": "not available: the host runs its own tools",
                }});
                self.write(&no).await?;
                continue;
            }
            return Ok(m);
        }
    }
}

/// What every thread runs with: no approvals asked, nothing written.
fn thread_settings(cwd: &Path, model: Option<&str>) -> Value {
    let mut p = json!({
        "cwd": cwd.to_string_lossy(),
        "approvalPolicy": "never",
        "sandbox": "read-only",
    });
    if let Some(m) = model {
        p["model"] = json!(m);
    }
    p
}

/// Disable tools that ARK neither offers nor executes through its own approval boundary.
/// The RPC overlay splits keys on dots literally (quoted segments are not TOML here).
fn host_config(response: &Value) -> Result<Value, AppError> {
    let config = response
        .get("config")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            AppError::Broken("config/read returned no configuration; use isolated exec".into())
        })?;
    let mut overrides = json!({
        "features.shell_tool": false,
        "features.apps": false,
        "web_search": "disabled",
    });
    if let Some(servers) = config.get("mcp_servers").filter(|v| !v.is_null()) {
        let servers = servers.as_object().ok_or_else(|| {
            AppError::Broken("unsupported MCP configuration; use isolated exec".into())
        })?;
        for name in servers.keys() {
            if name.is_empty()
                || !name
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
            {
                return Err(AppError::Broken(
                    "unsupported MCP name in host-model configuration; use isolated exec".into(),
                ));
            }
            overrides[format!("mcp_servers.{name}.enabled")] = json!(false);
        }
    }
    Ok(overrides)
}

fn record_limits(limits: &Value) {
    if let Some(u) = crate::usage::from_codex(limits) {
        crate::usage::record(u);
    }
}

/// A turn's token counts. Codex counts cached input inside `inputTokens`.
fn turn_usage(u: &Value) -> Usage {
    let n = |k: &str| u[k].as_u64().unwrap_or(0) as u32;
    if u.is_null() {
        return Usage::default();
    }
    Usage::measured(n("inputTokens"), n("outputTokens")).with_cached(n("cachedInputTokens"))
}

fn error_text(e: &Value) -> String {
    e["message"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_model_configuration_keeps_credentials_and_provider_choices_out() {
        let settings = host_config(&json!({"config": {
            "model": "user-model", "model_provider": "custom",
            "mcp_servers": {"notes-server": {"url": "https://example.test", "token": "secret"}}
        }}))
        .unwrap();
        assert_eq!(settings["mcp_servers.notes-server.enabled"], false);
        assert_eq!(settings["features.shell_tool"], false);
        assert_eq!(settings["features.apps"], false);
        assert_eq!(settings["web_search"], "disabled");
        let text = settings.to_string();
        for private in ["secret", "example.test", "user-model", "custom"] {
            assert!(!text.contains(private));
        }
        assert!(host_config(&json!({})).is_err());
        assert!(host_config(&json!({"config":{"mcp_servers":{"name.with.dot":{}}}})).is_err());
    }

    #[test]
    fn usage_counts_cached_input_inside_input() {
        let u = turn_usage(
            &json!({"totalTokens":15664,"inputTokens":15640,"cachedInputTokens":15104,"outputTokens":24,"reasoningOutputTokens":0}),
        );
        assert_eq!(u, Usage::measured(15640, 24).with_cached(15104));
        assert_eq!(turn_usage(&Value::Null), Usage::default());
    }
}
