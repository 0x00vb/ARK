//! One task on one agent program: start it, open or load its session, send the brief, answer
//! its permission requests through ARK, report what it does, end it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Instant;

use ark_core::RiskTier;
use ark_model::Usage;
use ark_runtime::approval::ApprovalRequest;
use ark_runtime::harness::HarnessEvent;
use ark_runtime::worker::{Worker, WorkerJob, WorkerRun};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout};

use crate::{Entry, ModelSetting, BASE_ENV};

pub use ark_runtime::worker::{CHANGE, OTHER, RUN};

/// The longest message ARK reads from a program.
const MAX_LINE: u64 = 16 * 1024 * 1024;

/// Said before the brief when a task runs again in the program's own session after a restart.
const RESUMED: &str = "This task ran before and stopped when the program running it restarted. \
Continue from where it stopped: look at what is already done in the folder and do not do it again.";

/// An agent program as an ARK worker.
pub struct Acp {
    entry: Entry,
    program: PathBuf,
}

impl Acp {
    pub fn new(entry: Entry, program: PathBuf) -> Acp {
        Acp { entry, program }
    }
}

#[async_trait::async_trait]
impl Worker for Acp {
    fn name(&self) -> &str {
        self.entry.name
    }

    async fn run(&self, job: WorkerJob<'_>) -> Result<WorkerRun, String> {
        let title = self.entry.title;
        let folder = job
            .folder
            .canonicalize()
            .unwrap_or_else(|_| job.folder.to_path_buf());
        let model = job.model.or(self.entry.default_model);
        let mut cmd = tokio::process::Command::new(&self.program);
        cmd.args(self.entry.args);
        if let (ModelSetting::ConfigArg, Some(m)) = (self.entry.model, model) {
            cmd.arg("-c").arg(format!("model=\"{m}\""));
        }
        // Only who and where the person is: never ARK's keys, tokens or secrets.
        cmd.env_clear();
        for k in BASE_ENV.iter().chain(self.entry.env) {
            if let Some(v) = std::env::var_os(k) {
                cmd.env(k, v);
            }
        }
        if let Some(home) = &self.entry.isolated_home {
            cmd.env("HOME", home)
                .env("GROK_HOME", home)
                .env("GROK_AGENT_DASHBOARD", "0")
                .env("GROK_WORKFLOWS", "0");
        }
        // The adapter reads the person's settings from its home: it gets an empty one. The
        // program it starts gets the person's real home (its login, its sessions) through the
        // session's options, exactly as when the person runs it.
        // A private folder of this run (0700), gone when the run ends.
        let empty = tempfile::Builder::new()
            .prefix("ark-program-home")
            .tempdir()
            .map_err(|e| format!("{title} did not start: {e}"))?;
        let settings_home: Vec<(String, std::ffi::OsString)> = match self.entry.settings_dir {
            Some(var) => {
                cmd.env("HOME", empty.path()).env_remove(var);
                ["HOME", var]
                    .into_iter()
                    .filter_map(|k| std::env::var_os(k).map(|v| (k.to_string(), v)))
                    .collect()
            }
            None => Vec::new(),
        };
        cmd.current_dir(&folder)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        // Its own group, so stopping it stops the commands it started too.
        #[cfg(unix)]
        cmd.process_group(0);
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("{title} did not start: {e}"))?;
        let group = Group(child.id());
        let kept_session = job
            .kept
            .as_ref()
            .and_then(|k| k["session"].as_str())
            .map(str::to_string);
        (job.keep)(json!({ "group": group.0, "session": kept_session }));
        let stderr = Tail::new(child.stderr.take());
        let mut conn = Conn {
            stdin: child.stdin.take().ok_or("started without input")?,
            reader: BufReader::new(child.stdout.take().ok_or("started without output")?),
            next: 0,
        };
        let mut turn = Turn::new(&folder, title, self.entry.name == "grok");
        let ran = self
            .session(
                &job,
                &folder,
                &mut conn,
                &mut turn,
                kept_session,
                group.0,
                settings_home,
            )
            .await;
        let out = match ran {
            Ok(run) => Ok(run),
            Err(_) if job.cancel.is_cancelled() => {
                let _ = conn
                    .notify("session/cancel", json!({ "sessionId": turn.session }))
                    .await;
                Ok(WorkerRun {
                    answer: turn.answer(),
                    cost_micro_usd: turn.cost,
                    cancelled: true,
                })
            }
            Err(e) => Err(match stderr.text().await {
                t if t.trim().is_empty() => format!("{title} {e}"),
                t => format!("{title} {e} ({})", t.trim()),
            }),
        };
        drop(conn);
        drop(group);
        let _ = child.kill().await;
        out
    }

    fn stop_left(&self, kept: &Value) {
        if let Some(g) = kept["group"].as_u64() {
            stop_group_of(g as u32, &self.program);
        }
    }
}

impl Acp {
    #[allow(clippy::too_many_arguments)]
    async fn session(
        &self,
        job: &WorkerJob<'_>,
        folder: &Path,
        conn: &mut Conn,
        turn: &mut Turn,
        kept_session: Option<String>,
        group: Option<u32>,
        settings_home: Vec<(String, std::ffi::OsString)>,
    ) -> Result<WorkerRun, String> {
        let init = conn
            .call(
                "initialize",
                json!({
                    "protocolVersion": 1,
                    "clientCapabilities": { "fs": { "readTextFile": false, "writeTextFile": false }, "terminal": false },
                }),
                turn,
                job,
                Updates::Ignore,
            )
            .await?;
        let version = init["agentInfo"]["version"]
            .as_str()
            .or_else(|| init["_meta"]["agentVersion"].as_str())
            .unwrap_or("");
        tracing::info!(worker = self.entry.name, version, "started");
        let checked = self.entry.checked;
        if !checked.is_empty() && !checked.iter().any(|c| version.starts_with(c)) {
            return Err(format!(
                "is not used: ARK was not checked with this version of its connector ({} {}; checked: {}), and its changes could skip ARK's questions",
                self.entry.program,
                if version.is_empty() { "of unknown version" } else { version },
                checked
                    .iter()
                    .map(|c| c.trim_end_matches('.'))
                    .collect::<Vec<_>>()
                    .join(", "),
            ));
        }
        let mut meta = (self.entry.session_meta)();
        if let (ModelSetting::ClaudeMeta, Some(m)) =
            (self.entry.model, job.model.or(self.entry.default_model))
        {
            meta["claudeCode"]["options"]["model"] = json!(m);
        }
        for (var, real) in &settings_home {
            meta["claudeCode"]["options"]["env"][var] = json!(real.to_string_lossy());
        }
        let can_load = init["agentCapabilities"]["loadSession"].as_bool() == Some(true);
        let mut resumed = false;
        let mut opened = Value::Null;
        if let (Some(s), true) = (&kept_session, can_load) {
            // Its history replays as updates: not this run's work.
            match conn
                .call(
                    "session/load",
                    json!({ "sessionId": s, "cwd": folder, "mcpServers": [], "_meta": meta }),
                    turn,
                    job,
                    Updates::Ignore,
                )
                .await
            {
                Ok(r) => {
                    opened = r;
                    opened["sessionId"] = json!(s);
                    resumed = true;
                }
                Err(e) => tracing::info!(worker = self.entry.name, "could not load: {e}"),
            }
        }
        if !resumed {
            opened = conn
                .call(
                    "session/new",
                    json!({ "cwd": folder, "mcpServers": [], "_meta": meta }),
                    turn,
                    job,
                    Updates::Ignore,
                )
                .await?;
        }
        let sid = opened["sessionId"]
            .as_str()
            .ok_or("opened no session")?
            .to_string();
        turn.session = sid.clone();
        (job.keep)(json!({ "group": group, "session": sid }));
        if let (ModelSetting::ConfigOption, Some(model)) =
            (self.entry.model, job.model.or(self.entry.default_model))
        {
            conn.call(
                "session/set_config_option",
                json!({ "sessionId": sid, "configId": "model", "value": model }),
                turn,
                job,
                Updates::Ignore,
            )
            .await
            .map_err(|error| format!("would not use model `{model}` ({error})"))?;
        }
        // The mode that asks before a change, whatever the program's own settings say.
        if let Some(want) = self.entry.mode {
            if opened["modes"]["currentModeId"].as_str() != Some(want) {
                conn.call(
                    "session/set_mode",
                    json!({ "sessionId": sid, "modeId": want }),
                    turn,
                    job,
                    Updates::Ignore,
                )
                .await
                .map_err(|e| format!("would not ask before changes ({e})"))?;
            }
        }
        let text = if resumed {
            format!("{RESUMED}\n\n{}", job.brief)
        } else {
            job.brief.to_string()
        };
        let started = Instant::now();
        let done = conn
            .call(
                "session/prompt",
                json!({ "sessionId": sid, "prompt": [{ "type": "text", "text": text }] }),
                turn,
                job,
                Updates::Take,
            )
            .await?;
        if let Some(u) = done
            .get("usage")
            .or_else(|| done.get("_meta").and_then(|meta| meta.get("usage")))
            .filter(|u| u.is_object())
        {
            let n = |k: &str| u[k].as_u64().unwrap_or(0).min(u32::MAX as u64) as u32;
            let (input, read, write) = (
                n("inputTokens"),
                n("cachedReadTokens"),
                n("cachedWriteTokens").max(n("cacheCreationTokens")),
            );
            (job.on_event)(HarnessEvent::ModelCall {
                role: "reason".into(),
                model: job
                    .model
                    .or(self.entry.default_model)
                    .unwrap_or(self.entry.name)
                    .to_string(),
                usage: Usage {
                    prompt_tokens: input.saturating_add(read).saturating_add(write),
                    completion_tokens: n("outputTokens"),
                    measured: true,
                    cached_tokens: read,
                    cache_write_tokens: write,
                },
                elapsed_ms: started.elapsed().as_millis() as u64,
                first_token_ms: None,
                retries: 0,
            });
        }
        match done["stopReason"].as_str().unwrap_or("") {
            "end_turn" => Ok(WorkerRun {
                answer: turn.answer(),
                cost_micro_usd: turn.cost,
                cancelled: false,
            }),
            "cancelled" => Ok(WorkerRun {
                answer: turn.answer(),
                cost_micro_usd: turn.cost,
                cancelled: true,
            }),
            "max_tokens" => Err("stopped: it reached its output limit".into()),
            "max_turn_requests" => Err("stopped: it reached its step limit".into()),
            "refusal" => Err("refused the task".into()),
            other => Err(format!("stopped ({other})")),
        }
    }
}

/// What to do with the program's updates while a call waits.
#[derive(Clone, Copy, PartialEq)]
enum Updates {
    Take,
    /// A loaded session's history, or setup: not this run's work.
    Ignore,
}

struct Conn {
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
    next: u64,
}

impl Conn {
    async fn send(&mut self, v: &Value) -> Result<(), String> {
        let mut line = v.to_string();
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| format!("stopped taking input ({e})"))
    }

    /// The program's next message, `None` when it closed its output.
    async fn next_line(&mut self) -> Result<Option<String>, String> {
        let mut buf = Vec::new();
        let n = (&mut self.reader)
            .take(MAX_LINE)
            .read_until(b'\n', &mut buf)
            .await
            .map_err(|e| e.to_string())?;
        if n == 0 {
            return Ok(None);
        }
        if n as u64 == MAX_LINE && buf.last() != Some(&b'\n') {
            return Err("sent a message too large to read".into());
        }
        Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        self.send(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
            .await
    }

    /// Send a request and handle what the program sends meanwhile, until its response.
    async fn call(
        &mut self,
        method: &str,
        params: Value,
        turn: &mut Turn,
        job: &WorkerJob<'_>,
        updates: Updates,
    ) -> Result<Value, String> {
        self.next += 1;
        let id = self.next;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
            .await?;
        loop {
            let line = tokio::select! {
                l = self.next_line() => l?.ok_or("stopped")?,
                _ = job.cancel.cancelled() => return Err("cancelled".into()),
            };
            let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let incoming = msg
                .get("method")
                .and_then(Value::as_str)
                .map(str::to_string);
            match incoming {
                None if msg["id"].as_u64() == Some(id) => {
                    return match msg.get("error") {
                        Some(e) => Err(format!(
                            "failed while {}: {}{}",
                            step(method),
                            e["message"].as_str().unwrap_or(&e.to_string()),
                            e["data"]["details"]
                                .as_str()
                                .map(|d| format!(" ({d})"))
                                .unwrap_or_default()
                        )),
                        None => Ok(msg["result"].clone()),
                    };
                }
                None => {}
                Some(m) if m == "session/update" => {
                    if updates == Updates::Take {
                        turn.update(&msg["params"]["update"], job);
                    }
                }
                Some(m) if m == "session/request_permission" => {
                    let outcome = tokio::select! {
                        o = turn.permission(&msg["params"], job) => o?,
                        _ = job.cancel.cancelled() => return Err("cancelled".into()),
                    };
                    self.send(&json!({ "jsonrpc": "2.0", "id": msg["id"], "result": { "outcome": outcome } }))
                        .await?;
                }
                // ARK serves nothing else (files, terminals): the program uses its own.
                Some(m) if msg.get("id").is_some() => {
                    self.send(&json!({ "jsonrpc": "2.0", "id": msg["id"], "error": { "code": -32601, "message": format!("{m} is not offered") } }))
                        .await?;
                }
                Some(_) => {}
            }
        }
    }
}

/// What one run said and did.
struct Turn {
    folder: PathBuf,
    /// The program, as a person calls it.
    by: &'static str,
    /// Grok is offered only with the permission shapes recorded in ADR 0061.
    strict_permissions: bool,
    session: String,
    text: String,
    since_tool: String,
    calls: HashMap<String, (String, String)>,
    cost: Option<u64>,
}

impl Turn {
    fn new(folder: &Path, by: &'static str, strict_permissions: bool) -> Turn {
        Turn {
            folder: folder.to_path_buf(),
            by,
            strict_permissions,
            session: String::new(),
            text: String::new(),
            since_tool: String::new(),
            calls: HashMap::new(),
            cost: None,
        }
    }

    /// Its last message: the text after its last tool call, else all it said.
    fn answer(&self) -> String {
        let a = if self.since_tool.trim().is_empty() {
            &self.text
        } else {
            &self.since_tool
        };
        a.trim().to_string()
    }

    fn update(&mut self, u: &Value, job: &WorkerJob<'_>) {
        match u["sessionUpdate"].as_str().unwrap_or("") {
            "agent_message_chunk" => {
                if let Some(t) = u["content"]["text"].as_str() {
                    self.text.push_str(t);
                    self.since_tool.push_str(t);
                }
            }
            "tool_call" => {
                self.since_tool.clear();
                let id = u["toolCallId"].as_str().unwrap_or("").to_string();
                let kind = u["kind"].as_str().unwrap_or("other").to_string();
                let title = u["title"].as_str().unwrap_or("").to_string();
                (job.on_event)(HarnessEvent::ToolCall {
                    id: id.clone(),
                    action: format!("program.{kind}"),
                    input: json!({ "title": title }),
                });
                self.calls.insert(id, (kind, title));
            }
            "tool_call_update" => {
                let id = u["toolCallId"].as_str().unwrap_or("");
                let status = u["status"].as_str().unwrap_or("");
                if let (Some((kind, title)), true) = (
                    self.calls.get_mut(id),
                    matches!(status, "completed" | "failed"),
                ) {
                    if let Some(t) = u["title"].as_str() {
                        *title = t.to_string();
                    }
                    (job.on_event)(HarnessEvent::ToolResult {
                        id: id.to_string(),
                        action: format!("program.{kind}"),
                        ok: status == "completed",
                        summary: title.clone(),
                        invocation: String::new(),
                        duration_ms: None,
                        error: (status == "failed").then(|| title.clone()),
                        context_tokens: 0,
                        raw_tokens: 0,
                        files: Vec::new(),
                        before: Vec::new(),
                    });
                }
            }
            "usage_update" => {
                if let Some(c) = u["cost"]["amount"].as_f64() {
                    self.cost = Some((c * 1_000_000.0).round().max(0.0) as u64);
                }
            }
            _ => {}
        }
    }

    /// A permission request, asked as an ARK question. ARK answers with the program's "once"
    /// options only: a grant is ARK's, kept by its gate, and names the file or the command.
    async fn permission(&mut self, params: &Value, job: &WorkerJob<'_>) -> Result<Value, String> {
        let request = request(&params["toolCall"], &self.folder, self.by);
        if self.strict_permissions && !known_grok_permission(&request) {
            return Err(format!(
                "sent an unrecognized permission request ({})",
                request.preview
            ));
        }
        let inside_change = request.action == CHANGE && !request.ask_each_time;
        let allowed = if job.allow_mutate && inside_change {
            true
        } else if let Some(gate) = &job.approvals {
            gate.check(&request).await.allows()
        } else {
            false
        };
        // Only the program's "once" answers: never one that changes its own rules or mode.
        let options = params["options"].as_array().cloned().unwrap_or_default();
        let once = |kind: &str| options.iter().find(|o| o["kind"] == kind);
        let pick = if allowed { once("allow_once") } else { None }
            .or_else(|| once("reject_once"))
            .or_else(|| {
                options
                    .iter()
                    .find(|o| o["kind"].as_str().is_some_and(|k| k.starts_with("reject")))
            });
        Ok(match pick {
            Some(o) => json!({ "outcome": "selected", "optionId": o["optionId"] }),
            None => json!({ "outcome": "cancelled" }),
        })
    }
}

fn known_grok_permission(request: &ApprovalRequest) -> bool {
    matches!(request.action.as_str(), CHANGE | RUN)
}

/// The ARK question for a program's permission request: what it wants to change or run, in
/// plain words, with a grant that names exactly that (ADR 0043 › Authority over work). `by` is
/// the program as a person calls it: the question says who acts, and where.
pub fn request(call: &Value, folder: &Path, by: &str) -> ApprovalRequest {
    let kind = call["kind"].as_str().unwrap_or("other");
    let title = call["title"].as_str().unwrap_or("something").to_string();
    let mut paths = Vec::new();
    let mut outside = false;
    for l in call["locations"].as_array().into_iter().flatten() {
        let Some(p) = l["path"].as_str() else {
            continue;
        };
        match relative(Path::new(p), folder) {
            Some(r) => paths.push(r),
            None => {
                outside = true;
                paths.push(p.to_string());
            }
        }
    }
    for p in [
        call["rawInput"]["file_path"].as_str(),
        call["_meta"]["x.ai/tool"]["input"]["path"].as_str(),
    ]
    .into_iter()
    .flatten()
    {
        match relative(Path::new(p), folder) {
            Some(r) => paths.push(r),
            None => {
                outside = true;
                paths.push(p.to_string());
            }
        }
    }
    paths.sort();
    paths.dedup();
    let command = match &call["rawInput"]["command"] {
        Value::String(s) => Some(s.clone()),
        Value::Array(a) => {
            let parts: Vec<&str> = a.iter().filter_map(Value::as_str).collect();
            Some(match parts.as_slice() {
                // A shell running a script: the script is the command.
                [shell, flag, script] if is_shell(shell) && matches!(*flag, "-c" | "-lc") => {
                    script.to_string()
                }
                _ => parts
                    .iter()
                    .map(|p| {
                        let plain = |c: char| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c);
                        if p.is_empty() || !p.chars().all(plain) {
                            format!("'{}'", p.replace('\'', "'\\''"))
                        } else {
                            p.to_string()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" "),
            })
        }
        _ => None,
    }
    .map(|c| without_cd_here(c.trim(), folder))
    .filter(|c| !c.is_empty());
    let base =
        |action: &str, input: Value, class: Option<String>, preview: String, summary: String| {
            ApprovalRequest {
                action: action.into(),
                summary,
                risk: RiskTier::Mutate,
                input,
                ask_each_time: class.is_none(),
                class: class.unwrap_or_else(|| action.to_string()),
                preview,
                agent: None,
                task: None,
                cut_short: false,
            }
        };
    match (kind, command) {
        ("edit" | "delete" | "move", _) if !paths.is_empty() => {
            let (verb, does) = match kind {
                "delete" => ("Remove", "removes"),
                "move" => ("Move", "moves"),
                _ => ("Change", "changes"),
            };
            let class = (!outside).then(|| format!("{CHANGE}:{}", paths.join(" ")));
            base(
                CHANGE,
                json!({ "paths": paths }),
                class,
                format!("{verb} {}{}", paths.join(", "), change_lines(call)),
                format!("{by} {does} it"),
            )
        }
        ("execute", Some(c)) => {
            let class = Some(format!("{RUN}:{c}@."));
            base(
                RUN,
                json!({ "command": c, "folder": "." }),
                class,
                format!("Run {c}"),
                format!("{by} runs it"),
            )
        }
        // Nothing concrete to name: asked every time, never granted.
        _ => base(
            OTHER,
            json!({ "title": title }),
            None,
            title,
            format!("{by} asks for this"),
        ),
    }
}

/// A command without a leading `cd` into the folder it already runs in (`cd "<folder>" &&
/// wc -w *.md` is `wc -w *.md`), so a grant for a command covers it. Any other `cd` stays.
fn without_cd_here(command: &str, folder: &Path) -> String {
    let Some(rest) = command.strip_prefix("cd ") else {
        return command.to_string();
    };
    let (dir, after) = match rest.chars().next() {
        Some(q @ ('"' | '\'')) => match rest[1..].split_once(q) {
            Some((d, a)) => (d.to_string(), a),
            None => return command.to_string(),
        },
        _ => match rest.split_once(' ') {
            Some((d, a)) => (d.to_string(), a),
            None => return command.to_string(),
        },
    };
    let Some(after) = after.trim_start().strip_prefix("&&") else {
        return command.to_string();
    };
    let here = Path::new(&dir)
        .canonicalize()
        .is_ok_and(|d| d == folder || dir == ".");
    if here {
        after.trim().to_string()
    } else {
        command.to_string()
    }
}

/// `sh`, `bash` or `zsh`, by name or path.
fn is_shell(program: &str) -> bool {
    matches!(
        program.rsplit('/').next().unwrap_or(program),
        "sh" | "bash" | "zsh" | "dash"
    )
}

/// What the program is doing when a call fails, in words.
fn step(method: &str) -> &'static str {
    match method {
        "initialize" => "starting",
        "session/new" => "opening its session",
        "session/load" => "loading its earlier session",
        "session/set_mode" => "setting the mode that asks before a change",
        "session/set_config_option" => "choosing its model",
        "session/prompt" => "working",
        _ => "talking to it",
    }
}

/// The lines a change removes and adds, from the diff a program sends with its request, under
/// the request's first line (a card shows them as code). At most 12.
fn change_lines(call: &Value) -> String {
    let mut out = Vec::new();
    for c in call["content"].as_array().into_iter().flatten() {
        if c["type"] != "diff" {
            continue;
        }
        let old: Vec<&str> = c["oldText"].as_str().unwrap_or("").lines().collect();
        let new: Vec<&str> = c["newText"].as_str().unwrap_or("").lines().collect();
        let head = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
        let tail = old[head..]
            .iter()
            .rev()
            .zip(new[head..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        for l in &old[head..old.len() - tail] {
            out.push(format!("  - {l}"));
        }
        for l in &new[head..new.len() - tail] {
            out.push(format!("  + {l}"));
        }
    }
    if out.is_empty() {
        return String::new();
    }
    let more = out.len().saturating_sub(12);
    out.truncate(12);
    if more > 0 {
        out.push(format!("  ({more} more lines)"));
    }
    format!("\n{}", out.join("\n"))
}

/// `path` relative to `folder`, `None` when it is outside it or reached through a link.
fn relative(path: &Path, folder: &Path) -> Option<String> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        folder.join(path)
    };
    if path
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
    {
        return None;
    }
    // A new file, maybe in new folders: the nearest folder that exists must be inside.
    let real = match path.canonicalize() {
        Ok(p) => p,
        Err(_) => {
            let mut missing = Vec::new();
            let mut at = path.as_path();
            loop {
                if let Ok(p) = at.canonicalize() {
                    break missing.iter().rev().fold(p, |p: PathBuf, m| p.join(m));
                }
                missing.push(at.file_name()?.to_os_string());
                at = at.parent()?;
            }
        }
    };
    let rel = real.strip_prefix(folder).ok()?;
    let parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    (!parts.is_empty() && !parts.iter().any(|p| p == "..")).then(|| parts.join("/"))
}

/// The program's process group: stopped with everything it started when dropped.
struct Group(Option<u32>);

impl Drop for Group {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(g) = self.0.filter(|g| *g > 1) {
            // SAFETY: killpg only sends a signal, to the group this run started.
            unsafe {
                libc::killpg(g as libc::pid_t, libc::SIGKILL);
            }
        }
    }
}

/// Stop a group a gone process left, if its leader is still the program it started (a process
/// id can be reused).
fn stop_group_of(group: u32, program: &Path) {
    #[cfg(unix)]
    {
        if group <= 1 {
            return;
        }
        // SAFETY: signal 0 sends nothing; it says whether the process exists.
        let leader_gone = unsafe { libc::kill(group as libc::pid_t, 0) } != 0;
        let name = program
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let leader = std::process::Command::new("ps")
            .args(["-o", "command=", "-p", &group.to_string()])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        // A group id is not given out again while the group has members: with its leader gone,
        // what is left in it is what the program started.
        if leader_gone || (!name.is_empty() && leader.contains(&name)) {
            // SAFETY: as in `Group::drop`; the leader is the program this task started.
            unsafe {
                libc::killpg(group as libc::pid_t, libc::SIGKILL);
            }
        }
    }
    #[cfg(not(unix))]
    let _ = (group, program);
}

/// The end of what the program wrote to its error stream, for a failure's reason.
struct Tail(tokio::task::JoinHandle<String>);

impl Tail {
    fn new(err: Option<tokio::process::ChildStderr>) -> Tail {
        Tail(tokio::spawn(async move {
            let Some(mut err) = err else {
                return String::new();
            };
            let mut keep = Vec::new();
            let mut buf = [0u8; 4096];
            while let Ok(n) = err.read(&mut buf).await {
                if n == 0 {
                    break;
                }
                keep.extend_from_slice(&buf[..n]);
                if keep.len() > 4000 {
                    keep.drain(..keep.len() - 2000);
                }
            }
            String::from_utf8_lossy(&keep).into_owned()
        }))
    }

    /// What it wrote so far (the program is stopped, or about to be).
    async fn text(self) -> String {
        match tokio::time::timeout(std::time::Duration::from_millis(300), self.0).await {
            Ok(Ok(t)) => t
                .chars()
                .rev()
                .take(400)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect(),
            _ => String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn grok_rejects_unknown_permission_shapes_before_approval() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let keep = |_: Value| {};
        let on_event = |_: HarnessEvent| {};
        let job = WorkerJob {
            brief: "Fetch the page",
            folder: &root,
            model: None,
            approvals: None,
            allow_mutate: false,
            cancel: ark_runtime::cancel::CancelToken::new(),
            kept: None,
            keep: &keep,
            on_event: &on_event,
        };
        let mut turn = Turn::new(&root, "Grok", true);

        let error = turn
            .permission(
                &json!({
                    "toolCall": { "kind": "fetch", "title": "Fetch a page" },
                    "options": [
                        { "kind": "allow_once", "optionId": "yes" },
                        { "kind": "reject_once", "optionId": "no" }
                    ]
                }),
                &job,
            )
            .await
            .unwrap_err();

        assert!(error.contains("unrecognized permission request"), "{error}");
    }

    #[test]
    fn requests_name_the_file_or_the_command() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir(root.join("web")).unwrap();
        let edit = request(
            &json!({ "kind": "edit", "title": "Write report.md", "locations": [
                { "path": root.join("report.md") }, { "path": root.join("web/a.md") } ] }),
            &root,
            "Fake",
        );
        assert_eq!(edit.action, CHANGE);
        assert_eq!(edit.class, "program.change:report.md web/a.md");
        assert_eq!(edit.preview, "Change report.md, web/a.md");
        assert_eq!(edit.summary, "Fake changes it");
        assert!(!edit.ask_each_time);

        let run = request(
            &json!({ "kind": "execute", "title": "x", "rawInput": { "command": "npm  test" } }),
            &root,
            "Fake",
        );
        assert_eq!(run.class, "program.run:npm  test@.", "the exact command");
        assert_eq!(run.preview, "Run npm  test");
        // Commands that differ only in spaces or lines are different commands.
        let two = request(
            &json!({ "kind": "execute", "title": "x", "rawInput": { "command": "echo hi\nrm -rf build" } }),
            &root,
            "Fake",
        );
        let one = request(
            &json!({ "kind": "execute", "title": "x", "rawInput": { "command": "echo hi rm -rf build" } }),
            &root,
            "Fake",
        );
        assert_ne!(two.class, one.class);
        // A shell running a script shows the script; other arguments keep their quotes.
        let script = request(
            &json!({ "kind": "execute", "title": "x", "rawInput": { "command": ["/bin/zsh", "-lc", "test -f a.md && cat a.md"] } }),
            &root,
            "Fake",
        );
        assert_eq!(script.preview, "Run test -f a.md && cat a.md");
        let args = request(
            &json!({ "kind": "execute", "title": "x", "rawInput": { "command": ["grep", "two words", "a.md"] } }),
            &root,
            "Fake",
        );
        assert_eq!(args.preview, "Run grep 'two words' a.md");
        let python = request(
            &json!({ "kind": "execute", "title": "x", "rawInput": { "command": ["python3", "-c", "print(1)"] } }),
            &root,
            "Fake",
        );
        assert_eq!(python.preview, "Run python3 -c 'print(1)'", "not a shell");
        // A `cd` into the folder it runs in anyway is the same command; any other is not.
        let here = request(
            &json!({ "kind": "execute", "title": "x",
                "rawInput": { "command": format!("cd \"{}\" && npm  test", root.display()) } }),
            &root,
            "Fake",
        );
        assert_eq!(here.class, "program.run:npm  test@.");
        let elsewhere = request(
            &json!({ "kind": "execute", "title": "x", "rawInput": { "command": "cd web && npm test" } }),
            &root,
            "Fake",
        );
        assert_eq!(elsewhere.class, "program.run:cd web && npm test@.");

        // Outside the folder, or nothing to name: asked every time.
        let outside = request(
            &json!({ "kind": "edit", "title": "x", "locations": [{ "path": "/etc/hosts" }] }),
            &root,
            "Fake",
        );
        assert!(outside.ask_each_time);
        let other = request(
            &json!({ "kind": "fetch", "title": "Fetch a page" }),
            &root,
            "Fake",
        );
        assert_eq!(other.action, OTHER);
        assert!(other.ask_each_time);
        assert!(!known_grok_permission(&other));
        assert!(known_grok_permission(&run));
        let up = request(
            &json!({ "kind": "edit", "title": "x", "locations": [{ "path": root.join("../x.md") }] }),
            &root,
            "Fake",
        );
        assert!(up.ask_each_time);

        let grok_edit = request(
            &json!({ "kind": "edit", "title": "Edit report", "rawInput": {
                "file_path": root.join("report.md")
            } }),
            &root,
            "Grok",
        );
        assert_eq!(grok_edit.class, "program.change:report.md");
        let grok_meta = request(
            &json!({ "kind": "edit", "title": "Edit nested", "_meta": {
                "x.ai/tool": { "input": { "path": root.join("web/a.md") } }
            } }),
            &root,
            "Grok",
        );
        assert_eq!(grok_meta.class, "program.change:web/a.md");
    }
}
