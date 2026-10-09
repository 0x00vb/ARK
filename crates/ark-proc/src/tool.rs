//! The `process` tool: start, list and stop project processes.

use std::sync::Arc;
use std::time::Duration;

use ark_core::{ActionDescriptor, RiskTier};
use ark_tools::{ActionOutput, Adapter, AdapterError, RunCtx};
use async_trait::async_trait;
use serde_json::{json, Value as Json};

use crate::{Info, Processes, Start};

pub const START: &str = "process.start";
pub const LIST: &str = "process.list";
pub const STOP: &str = "process.stop";

/// How long a start waits for ready unless the agent asks for less or more, and the most.
const WAIT: u64 = 30;
const MAX_WAIT: u64 = 300;
/// Output the agent gets with a start, and with `process.list {id}`.
const START_TAIL: usize = 2_000;
const READ_TAIL: usize = 6_000;

/// The tool, over a runtime's [`Processes`].
pub struct ProcessTool(pub Processes);

fn bad(action: &str, detail: impl Into<String>) -> AdapterError {
    AdapterError::BadInput {
        action: action.into(),
        detail: detail.into(),
    }
}

fn info_json(i: &Info) -> Json {
    serde_json::to_value(i).unwrap_or(Json::Null)
}

impl ProcessTool {
    async fn start(&self, input: &Json, ctx: &RunCtx<'_>) -> Result<ActionOutput, AdapterError> {
        let root = ctx
            .workspace
            .clone()
            .ok_or_else(|| bad(START, "there is no project folder to run in"))?;
        let s = |k: &str| {
            input
                .get(k)
                .and_then(Json::as_str)
                .unwrap_or("")
                .to_string()
        };
        let command = s("command");
        if direct_desktop_automation(&command) {
            return Err(bad(
                START,
                "project processes do not open or control desktop apps; use the Computer capability",
            ));
        }
        let req = Start {
            command,
            folder: s("folder"),
            port: input
                .get("port")
                .and_then(Json::as_u64)
                .and_then(|p| u16::try_from(p).ok()),
            open: input.get("open").and_then(Json::as_str).map(str::to_string),
        };
        let wait = input
            .get("wait")
            .and_then(Json::as_u64)
            .unwrap_or(WAIT)
            .min(MAX_WAIT);
        let already = self.0.running_like(
            &root,
            req.folder.trim().trim_start_matches("./").trim_matches('/'),
            &req.command,
        );
        let started = self
            .0
            .start(&root, &req)
            .await
            .map_err(|e| AdapterError::Tool(e.to_string()))?;
        let now = self
            .0
            .settle(&started.id, Duration::from_secs(wait))
            .await
            .unwrap_or(started);
        let output = self.0.output(&now.id, START_TAIL).unwrap_or_default();
        let mut out = info_json(&now);
        out["already_running"] = json!(already.is_some());
        out["output"] = json!(output);
        Ok(ActionOutput::json(out).with_command(vec![now.command.clone()]))
    }

    fn list(&self, input: &Json) -> Result<ActionOutput, AdapterError> {
        if let Some(id) = input
            .get("id")
            .and_then(Json::as_str)
            .filter(|s| !s.is_empty())
        {
            let info = self
                .0
                .get(id)
                .ok_or_else(|| bad(LIST, format!("there is no process `{id}`")))?;
            let mut out = info_json(&info);
            out["output"] = json!(self.0.output(id, READ_TAIL).unwrap_or_default());
            return Ok(ActionOutput::json(out));
        }
        let all: Vec<Json> = self.0.list().iter().map(info_json).collect();
        Ok(ActionOutput::json(json!({ "processes": all })))
    }

    async fn stop(&self, input: &Json) -> Result<ActionOutput, AdapterError> {
        let id = input
            .get("id")
            .and_then(Json::as_str)
            .ok_or_else(|| bad(STOP, "`id` is the process to stop (process.list)"))?;
        let info = self
            .0
            .stop(id)
            .await
            .ok_or_else(|| bad(STOP, format!("there is no process `{id}`")))?;
        Ok(ActionOutput::json(info_json(&info)))
    }
}

/// A usability guard for the common mistake seen in the WhatsApp trace. Project processes are
/// intentionally not a sandbox, so this rejects direct forms only and makes no security claim.
fn direct_desktop_automation(command: &str) -> bool {
    fn executable(word: &str) -> &str {
        word.rsplit('/').next().unwrap_or(word)
    }
    let lower = command.trim().to_ascii_lowercase();
    let words: Vec<&str> = lower.split_whitespace().collect();
    let mut at = 0;
    if words.get(at).is_some_and(|w| executable(w) == "env") {
        at += 1;
        while words
            .get(at)
            .is_some_and(|w| w.contains('=') && !w.starts_with('='))
        {
            at += 1;
        }
    }
    match words.get(at).map(|w| executable(w)) {
        Some("osascript") => true,
        Some("open") => words[at + 1..]
            .iter()
            .any(|w| matches!(*w, "-a" | "--application")),
        Some("sh" | "bash" | "zsh") => {
            let rest = words[at + 1..].join(" ");
            rest.contains("osascript")
                || (rest.contains("open")
                    && (rest.contains(" -a ") || rest.contains(" --application ")))
        }
        _ => false,
    }
}

#[async_trait]
impl Adapter for ProcessTool {
    fn name(&self) -> &str {
        "process"
    }

    fn summary(&self) -> Option<&str> {
        Some("run the project's own commands with the person's shell: tests, builds, dev servers")
    }

    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![
            ActionDescriptor::new(
                START,
                "Run a command in the project with the person's own shell, programs and settings (their PATH: cargo, npm, python and the rest): the project's tests, a build, a dev server, a watcher. It keeps running after this call; the person sees its output and can stop it. Answers once it is ready (a local address in its output answers), has ended (with its exit code), or `wait` passed, with the end of its output; `process.list` with its id reads more. For tests or a build, pass a `wait` long enough for it to end. To show the person its page, pass `open`.",
                RiskTier::Mutate,
            )
            // "Always in this project" covers this command in this folder, never another.
            .grant_per(START, &["command", "folder"])
            .with_input_schema(json!({
                "type": "object", "required": ["command"],
                "properties": {
                    "command": { "type": "string", "description": "the command line, as typed in a terminal (\"npm run dev\")" },
                    "folder": { "type": "string", "description": "where to run it, inside the project (\"web\"); default the project folder" },
                    "port": { "type": "integer", "description": "a port that answers when it is ready, when its output prints no address" },
                    "open": { "type": "string", "description": "a path to open in the person's browser view once ready (\"/home\")" },
                    "wait": { "type": "integer", "description": format!("seconds to wait for ready, default {WAIT}, at most {MAX_WAIT}") }
                }
            })),
            ActionDescriptor::new(
                LIST,
                "The project processes: each one's state and address. With `id`, that one with the end of its output (errors, the address).",
                RiskTier::Observe,
            )
            .volatile()
            .with_input_schema(json!({
                "type": "object",
                "properties": { "id": { "type": "string" } }
            })),
            ActionDescriptor::new(
                STOP,
                "Stop a process ARK started, with everything it started.",
                RiskTier::Observe,
            )
            .volatile()
            .with_input_schema(json!({
                "type": "object", "required": ["id"],
                "properties": { "id": { "type": "string" } }
            })),
        ]
    }

    async fn run(
        &self,
        action: &str,
        input: Json,
        ctx: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        match action {
            START => self.start(&input, ctx).await,
            LIST => self.list(&input),
            STOP => self.stop(&input).await,
            _ => Err(AdapterError::UnknownAction(action.into())),
        }
    }
}

/// What the agent reads of a process call: its state, address and the end of its output.
pub fn distill() -> ark_runtime::distill::Distiller {
    Arc::new(|action: &str, out: &Json| {
        if ![START, LIST, STOP].contains(&action) {
            return None;
        }
        if let Some(all) = out.get("processes").and_then(Json::as_array) {
            if all.is_empty() {
                return Some(("no processes".into(), Vec::new()));
            }
            let lines: Vec<String> = all.iter().map(line).collect();
            return Some((lines.join("\n"), Vec::new()));
        }
        let mut t = line(out);
        if out["already_running"] == true {
            t.push_str(" (it was already running)");
        }
        if out["state"] == "running" && action == START {
            t.push_str(
                "; no address answers yet (process.list with its id shows its output later)",
            );
        }
        if let Some(o) = out.get("output").and_then(Json::as_str).map(str::trim_end) {
            if !o.is_empty() {
                t.push_str(&format!("\noutput:\n{o}"));
            }
        }
        Some((t, Vec::new()))
    })
}

/// `p1 npm run dev (in web): ready at http://localhost:3000/`
fn line(p: &Json) -> String {
    let s = |k: &str| p.get(k).and_then(Json::as_str).unwrap_or("");
    let mut t = format!("{} {}", s("id"), s("command"));
    if !s("folder").is_empty() {
        t.push_str(&format!(" (in {})", s("folder")));
    }
    t.push_str(": ");
    t.push_str(&match s("state") {
        "ready" => format!("ready at {}", s("url")),
        "exited" => format!("ended, exit {}", p["code"].as_i64().unwrap_or(-1)),
        "stopped" => "stopped".into(),
        _ if !s("url").is_empty() => format!("running, address {}", s("url")),
        _ => "running".into(),
    });
    t
}

#[cfg(test)]
mod guard_tests {
    use super::direct_desktop_automation;

    #[test]
    fn common_direct_desktop_commands_use_computer_instead() {
        for command in [
            "open -a WhatsApp",
            "/usr/bin/open --application WhatsApp",
            "osascript -e 'tell application \"WhatsApp\" to activate'",
            "/usr/bin/env LANG=C /usr/bin/osascript script.scpt",
            "env FOO=bar open -a WhatsApp",
            "sh -c 'open -a WhatsApp'",
            "/bin/zsh -c '/usr/bin/osascript -e x'",
        ] {
            assert!(direct_desktop_automation(command), "{command}");
        }
        for command in [
            "cargo test",
            "npm run dev",
            "openapi generate",
            "echo osascript",
        ] {
            assert!(!direct_desktop_automation(command), "{command}");
        }
    }
}
