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
    account: Option<crate::accounts::ModelAccount>,
    proc: LiveProcess,
    next_id: u64,
    /// Threads this process has started or loaded.
    threads: std::collections::HashSet<String>,
    host_config: Option<Value>,
    isolation: Option<std::sync::Arc<crate::codex_host::CodexHost>>,
    default_model: Option<String>,
    token_totals: std::collections::HashMap<String, TokenTotals>,
    active_turn: Option<(String, String)>,
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
    #[cfg(test)]
    pub(crate) async fn start(bin: &Path, version: &str) -> Result<Self, AppError> {
        Self::start_for(bin, version, None).await
    }
    pub(crate) async fn start_for(
        bin: &Path,
        version: &str,
        account: Option<&crate::accounts::ModelAccount>,
    ) -> Result<Self, AppError> {
        let mut app = Self::start_catalog_for(bin, version, account).await?;
        // The account's usage limits, before any turn. A sign-in without them (an API key)
        // answers with an error, which changes nothing.
        if let Ok(r) = app
            .request("account/rateLimits/read", Value::Null, SETUP_TIMEOUT)
            .await
        {
            app.record_limits(&r);
        }
        Ok(app)
    }

    /// Start only the process and handshake. Catalog discovery needs no usage query.
    pub(crate) async fn start_catalog_for(
        bin: &Path,
        version: &str,
        account: Option<&crate::accounts::ModelAccount>,
    ) -> Result<Self, AppError> {
        Self::start_configured(bin, version, account, &[], None).await
    }

    pub(crate) async fn start_restricted_catalog(
        bin: &Path,
        version: &str,
        account: Option<&crate::accounts::ModelAccount>,
        flags: &[String],
        cwd: &Path,
    ) -> Result<Self, AppError> {
        Self::start_configured(bin, version, account, flags, Some(cwd)).await
    }

    pub(crate) async fn start_qualified(
        isolation: std::sync::Arc<crate::codex_host::CodexHost>,
        version: &str,
        account: Option<&crate::accounts::ModelAccount>,
    ) -> Result<Self, AppError> {
        isolation.fast_verify().map_err(AppError::Failed)?;
        let mut app = Self::start_restricted_catalog(
            isolation.native(),
            version,
            account,
            &isolation.flags(),
            isolation.directory(),
        )
        .await?;
        isolation.fast_verify().map_err(AppError::Failed)?;
        app.isolation = Some(isolation);
        app.prepare_host().await?;
        Ok(app)
    }

    async fn start_configured(
        bin: &Path,
        version: &str,
        account: Option<&crate::accounts::ModelAccount>,
        flags: &[String],
        cwd: Option<&Path>,
    ) -> Result<Self, AppError> {
        if account.is_some_and(|a| a.provider != "codex") {
            return Err(AppError::Failed(
                "This account belongs to another provider.".into(),
            ));
        }
        let mut cmd = if flags.is_empty() {
            let mut cmd = crate::cli::command(bin);
            if let Some(account) = account {
                account
                    .setup_command(&mut cmd)
                    .map_err(|e| AppError::Failed(e.to_string()))?;
            }
            cmd
        } else {
            crate::codex_host::restricted_command(bin, account).map_err(AppError::Failed)?
        };
        cmd.args(["app-server", "-c", "sandbox_mode=\"read-only\""]);
        crate::cli::host_codex_flags(&mut cmd, false);
        cmd.args(flags);
        if let Some(cwd) = cwd {
            cmd.current_dir(cwd);
        }
        let proc = LiveProcess::spawn(cmd)
            .map_err(|e| AppError::Broken(format!("could not start: {e}")))?;
        let mut app = CodexApp {
            account: account.cloned(),
            proc,
            next_id: 0,
            threads: Default::default(),
            host_config: None,
            isolation: None,
            default_model: None,
            token_totals: Default::default(),
            active_turn: None,
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

    fn record_limits(&self, limits: &Value) {
        if let Some(u) = crate::usage::from_codex_response(limits) {
            if let Some(account) = &self.account {
                account.record(u);
            } else {
                crate::usage::record(u);
            }
        }
    }

    pub(crate) fn qualified_for(
        &self,
        isolation: &std::sync::Arc<crate::codex_host::CodexHost>,
    ) -> bool {
        self.isolation
            .as_ref()
            .is_some_and(|saved| std::sync::Arc::ptr_eq(saved, isolation))
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
                json!({"includeLayers": self.isolation.is_some()}),
                SETUP_TIMEOUT,
            )
            .await?;
        if let Some(isolation) = &self.isolation {
            isolation
                .verify_configuration(&response)
                .map_err(AppError::Failed)?;
        }
        let config = host_config(&response)?;
        self.host_config = Some(config.clone());
        Ok(config)
    }

    pub(crate) async fn identity(&mut self) -> Result<Option<String>, AppError> {
        let response = self
            .request(
                "account/read",
                json!({"refreshToken": false}),
                SETUP_TIMEOUT,
            )
            .await?;
        Ok(response["account"]["email"].as_str().map(str::to_string))
    }

    pub(crate) async fn account_fingerprint(&mut self) -> Result<blake3::Hash, AppError> {
        let response = self
            .request("account/read", json!({"refreshToken":false}), SETUP_TIMEOUT)
            .await?;
        crate::codex_host::account_fingerprint(&response).map_err(AppError::Failed)
    }

    /// Models and effort levels available to this signed-in Codex account.
    pub(crate) async fn models(&mut self) -> Result<Vec<ModelOption>, AppError> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        let mut seen = std::collections::HashSet::new();
        for _ in 0..20 {
            let r = self
                .request(
                    "model/list",
                    json!({"cursor": cursor, "includeHidden": false}),
                    SETUP_TIMEOUT,
                )
                .await?;
            let invalid = || AppError::Broken("model/list returned an invalid catalog page".into());
            let models = r
                .get("data")
                .and_then(Value::as_array)
                .ok_or_else(invalid)?;
            for model in models {
                let model = model.as_object().ok_or_else(invalid)?;
                if let Some(hidden) = model.get("hidden") {
                    if hidden.as_bool().ok_or_else(invalid)? {
                        continue;
                    }
                }
                let id = model
                    .get("model")
                    .or_else(|| model.get("id"))
                    .and_then(Value::as_str)
                    .filter(|id| !id.trim().is_empty())
                    .ok_or_else(invalid)?;
                let label = model
                    .get("displayName")
                    .map(|label| label.as_str().ok_or_else(invalid))
                    .transpose()?
                    .unwrap_or(id)
                    .to_string();
                let efforts = model
                    .get("supportedReasoningEfforts")
                    .map(|efforts| {
                        efforts
                            .as_array()
                            .ok_or_else(invalid)?
                            .iter()
                            .map(|effort| {
                                effort
                                    .get("reasoningEffort")
                                    .and_then(Value::as_str)
                                    .filter(|effort| !effort.trim().is_empty())
                                    .map(str::to_string)
                                    .ok_or_else(invalid)
                            })
                            .collect::<Result<Vec<_>, _>>()
                    })
                    .transpose()?
                    .unwrap_or_default();
                let default_effort = model
                    .get("defaultReasoningEffort")
                    .filter(|value| !value.is_null())
                    .map(|value| {
                        value
                            .as_str()
                            .filter(|effort| !effort.trim().is_empty())
                            .map(str::to_string)
                            .ok_or_else(invalid)
                    })
                    .transpose()?;
                if model.get("isDefault").and_then(Value::as_bool) == Some(true) {
                    self.default_model = Some(id.to_string());
                }
                out.push(ModelOption {
                    id: id.to_string(),
                    label,
                    efforts,
                    default_effort,
                });
            }
            match r.get("nextCursor") {
                None | Some(Value::Null) => return Ok(out),
                Some(Value::String(next)) if !next.is_empty() && seen.insert(next.clone()) => {
                    cursor = Some(next.clone());
                }
                _ => {
                    return Err(AppError::Broken(
                        "model/list returned an invalid or repeated cursor".into(),
                    ))
                }
            }
        }
        Err(AppError::Broken(
            "model/list exceeded the catalog page limit".into(),
        ))
    }

    pub(crate) async fn qualification_snapshot(
        &mut self,
    ) -> Result<(Vec<ModelOption>, Option<String>), AppError> {
        let models = self
            .models()
            .await
            .map_err(|_| AppError::Failed("Codex model catalog is unavailable".into()))?;
        let response = self
            .request("config/read", json!({"includeLayers":true}), SETUP_TIMEOUT)
            .await
            .map_err(|_| AppError::Failed("Codex configuration is unavailable".into()))?;
        crate::codex_host::verify_startup_configuration(&response).map_err(AppError::Failed)?;
        host_config(&response)?;
        if response["config"]["model_provider"]
            .as_str()
            .is_some_and(|provider| provider != "openai")
        {
            return Err(AppError::Failed(
                "Protected Codex requires the official subscription provider".into(),
            ));
        }
        let default = response["config"]["model"]
            .as_str()
            .map(str::to_string)
            .or_else(|| self.default_model.clone());
        Ok((models, default))
    }

    /// Start a thread: no approvals, read-only, `instructions` as its base instructions.
    pub(crate) async fn start_thread(
        &mut self,
        cwd: &Path,
        model: Option<&str>,
        instructions: &str,
    ) -> Result<String, AppError> {
        self.start_thread_ephemeral(cwd, model, instructions, false)
            .await
    }

    pub(crate) async fn start_thread_ephemeral(
        &mut self,
        cwd: &Path,
        model: Option<&str>,
        instructions: &str,
        ephemeral: bool,
    ) -> Result<String, AppError> {
        let mut p = thread_settings(cwd, model);
        p["config"] = self.prepare_host().await?;
        p["baseInstructions"] = json!(instructions);
        p["ephemeral"] = json!(ephemeral);
        let r = self.request("thread/start", p, SETUP_TIMEOUT).await?;
        let id = r["thread"]["id"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| AppError::Broken("thread/start returned no thread id".into()))?;
        self.threads.insert(id.clone());
        self.token_totals.insert(id.clone(), TokenTotals::default());
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
        if self.isolation.is_some() {
            self.request(
                "thread/read",
                json!({"threadId":id,"includeTurns":false}),
                SETUP_TIMEOUT,
            )
            .await?;
            if !self.token_totals.contains_key(id) {
                return Err(AppError::Failed(
                    "Start a new chat to continue with this model".into(),
                ));
            }
        }
        self.threads.insert(id.to_string());
        Ok(())
    }

    /// Run one turn on thread `id`, its reply shaped by `schema`. The reply's `text` field
    /// reaches `on_text` while it is written.
    pub(crate) async fn turn(&mut self, request: TurnRequest<'_>) -> Result<Turn, AppError> {
        self.active_turn = None;
        let thread = request.thread_id.to_string();
        let before = self.token_totals.get(&thread).copied();
        let result = self.turn_inner(request).await;
        if result.is_err() {
            let partial = self
                .token_totals
                .get(&thread)
                .copied()
                .zip(before)
                .and_then(|(after, before)| after.delta(before).ok());
            tracing::warn!(prompt_tokens=partial.map(|usage|usage.prompt_tokens),completion_tokens=partial.map(|usage|usage.completion_tokens),cached_tokens=partial.map(|usage|usage.cached_tokens),"Codex did not complete; spending may be partial or unknown and the call is not automatically retried");
        }
        self.active_turn = None;
        result
    }

    async fn turn_inner(&mut self, request: TurnRequest<'_>) -> Result<Turn, AppError> {
        let TurnRequest {
            thread_id: id,
            input,
            pictures,
            schema,
            effort,
            wait,
            on_text,
        } = request;
        if let Some(isolation) = self.isolation.clone() {
            isolation.verify_account().await.map_err(AppError::Failed)?;
            isolation
                .matches_account(self.account_fingerprint().await?)
                .map_err(AppError::Failed)?;
        }
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
        let mut baseline = self.token_totals.get(id).copied();
        let mut previous = baseline;
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
                if let Some(turn) = m["result"]["turn"]["id"].as_str() {
                    self.active_turn = Some((id.to_string(), turn.to_string()));
                }
                if baseline.is_none() {
                    baseline = self.token_totals.get(id).copied();
                    previous = baseline;
                }
                continue;
            }
            let params = &m["params"];
            if params["threadId"].as_str().is_some_and(|t| t != id) {
                continue;
            }
            if let Some((_, active)) = &self.active_turn {
                if params["turnId"].as_str().is_some_and(|turn| turn != active)
                    || (m["method"] == "turn/completed"
                        && params["turn"]["id"]
                            .as_str()
                            .is_some_and(|turn| turn != active))
                {
                    continue;
                }
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
                Some("account/rateLimits/updated") => self.record_limits(params),
                Some("thread/tokenUsage/updated") => {
                    if let Some(total) = TokenTotals::parse(&params["tokenUsage"]["total"])? {
                        if let Some(previous) = previous {
                            total.delta(previous)?;
                        }
                        previous = Some(total);
                        if let Some(baseline) = baseline {
                            usage = total.delta(baseline)?;
                        } else if self.isolation.is_some() {
                            return Err(AppError::Broken(
                                "Codex could not report this turn's usage. Try again.".into(),
                            ));
                        } else {
                            usage = Usage::default();
                        }
                    } else if self.isolation.is_some() {
                        usage = Usage::default();
                    } else {
                        usage = turn_usage(&params["tokenUsage"]["last"]);
                    }
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
            if m["method"] == "thread/tokenUsage/updated" {
                let params = &m["params"];
                if let Some(thread) = params["threadId"].as_str() {
                    let belongs =
                        self.active_turn
                            .as_ref()
                            .is_none_or(|(active_thread, active_turn)| {
                                thread != active_thread
                                    || params["turnId"].as_str() == Some(active_turn)
                            });
                    if belongs {
                        if let Some(total) = TokenTotals::parse(&params["tokenUsage"]["total"])? {
                            self.token_totals.insert(thread.to_string(), total);
                        }
                    }
                }
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

#[derive(Clone, Copy, Default)]
struct TokenTotals {
    input: u64,
    output: u64,
    cached: u64,
    written: u64,
}
impl TokenTotals {
    fn parse(value: &Value) -> Result<Option<Self>, AppError> {
        if value.is_null() {
            return Ok(None);
        }
        let number = |name: &str| {
            value[name]
                .as_u64()
                .ok_or_else(|| AppError::Broken("Codex returned invalid token totals".into()))
        };
        Ok(Some(Self {
            input: number("inputTokens")?,
            output: number("outputTokens")?,
            cached: number("cachedInputTokens")?,
            written: if value.get("cacheWriteInputTokens").is_some() {
                number("cacheWriteInputTokens")?
            } else {
                0
            },
        }))
    }
    fn delta(self, baseline: Self) -> Result<Usage, AppError> {
        let difference = |current: u64, before: u64| {
            current
                .checked_sub(before)
                .and_then(|n| u32::try_from(n).ok())
                .ok_or_else(|| {
                    AppError::Broken("Codex could not report this turn's usage. Try again.".into())
                })
        };
        Ok(Usage::measured(
            difference(self.input, baseline.input)?,
            difference(self.output, baseline.output)?,
        )
        .with_cached(difference(self.cached, baseline.cached)?)
        .with_cache_write(difference(self.written, baseline.written)?))
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
    fn cumulative_usage_rejects_counter_resets_and_overflow() {
        let baseline = TokenTotals {
            input: 50,
            output: 18,
            cached: 15,
            written: 0,
        };
        assert!(TokenTotals {
            input: 49,
            ..baseline
        }
        .delta(baseline)
        .is_err());
        assert!(TokenTotals {
            input: u64::MAX,
            ..baseline
        }
        .delta(baseline)
        .is_err());
        let current = TokenTotals {
            input: 90,
            output: 31,
            cached: 35,
            written: 0,
        };
        assert_eq!(
            current.delta(baseline).unwrap(),
            Usage::measured(40, 13).with_cached(20)
        );
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
