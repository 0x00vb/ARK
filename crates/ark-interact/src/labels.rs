//! Turning tool calls into words a person reads. The runtime reports `manuscript.read` with a
//! JSON input; a person wants "Reading chapter-04.md". A consumer labels its own actions;
//! everything else gets a readable default from the action name and its main argument.
//! Every front-end uses the same labels.

use std::sync::Arc;

use ark_sdk::prelude::ark_proc::{
    LIST as PROCESS_LIST, START as PROCESS_START, STOP as PROCESS_STOP,
};
use ark_sdk::prelude::{ark_exec, ark_files, ApprovalRequest};
use serde_json::Value;

use crate::fmt::clip;

/// A Computer activity phase in words a person can act on. Front-ends draw these words; they do
/// not expose action names or runtime terms.
pub fn computer_activity_line(
    activity: &ark_sdk::prelude::ark_computer::ComputerActivity,
) -> String {
    use ark_sdk::prelude::ark_computer::ActivityPhase;
    match activity.phase {
        ActivityPhase::WaitingPermission => format!("Allow ARK to use {}", activity.app),
        ActivityPhase::WaitingPerson => "Waiting for you to finish".into(),
        ActivityPhase::Acting => format!("Using {}", activity.app),
        ActivityPhase::Verifying => format!("Checking {}", activity.app),
        ActivityPhase::Hazardous => format!("A draft in {} needs attention", activity.app),
        ActivityPhase::Stopped => "Stopped".into(),
        ActivityPhase::Refused => "That action was not allowed".into(),
        ActivityPhase::Failed => format!("Could not use {}", activity.app),
        ActivityPhase::SubmittedUnconfirmed => {
            format!("The message may have sent. Check {}", activity.app)
        }
        ActivityPhase::Confirmed => format!("Confirmed in {}", activity.app),
        ActivityPhase::Idle => String::new(),
    }
}

/// Custom wording for a consumer's tools. Every method is optional; `None` falls back to the
/// default.
pub trait ToolLabeler: Send + Sync {
    /// The activity label for a call, e.g. `Reading chapter-04.md`.
    fn call(&self, _action: &str, _input: &Value) -> Option<String> {
        None
    }
    /// The one-line result under a finished call. `summary` is the runtime's distilled result.
    fn result(&self, _action: &str, _ok: bool, _summary: &str) -> Option<String> {
        None
    }
    /// The one-line call shown when asking for approval, e.g. `$ cargo test --workspace`.
    fn approval(&self, _request: &ApprovalRequest) -> Option<String> {
        None
    }
}

type LabelFn = Arc<dyn Fn(&Value) -> String + Send + Sync>;

/// Per-action labels, matched by exact action name or by `namespace.` prefix.
#[derive(Clone, Default)]
pub struct ActivityLabels {
    exact: Vec<(String, LabelFn)>,
    prefix: Vec<(String, LabelFn)>,
}

impl ActivityLabels {
    pub fn new() -> Self {
        Self::default()
    }

    /// Label one action: `.label("manuscript.read", |i| format!("Reading {}", i["file"]))`.
    pub fn label(
        mut self,
        action: impl Into<String>,
        f: impl Fn(&Value) -> String + Send + Sync + 'static,
    ) -> Self {
        self.exact.push((action.into(), Arc::new(f)));
        self
    }

    /// Label every action in a namespace: `.namespace("notes", |_| "Reading notes".into())`.
    pub fn namespace(
        mut self,
        ns: impl Into<String>,
        f: impl Fn(&Value) -> String + Send + Sync + 'static,
    ) -> Self {
        let mut ns = ns.into();
        if !ns.ends_with('.') {
            ns.push('.');
        }
        self.prefix.push((ns, Arc::new(f)));
        self
    }

    fn find(&self, action: &str) -> Option<&LabelFn> {
        self.exact
            .iter()
            .find(|(a, _)| a == action)
            .map(|(_, f)| f)
            .or_else(|| {
                self.prefix
                    .iter()
                    .find(|(p, _)| action.starts_with(p.as_str()))
                    .map(|(_, f)| f)
            })
    }
}

impl ToolLabeler for ActivityLabels {
    fn call(&self, action: &str, input: &Value) -> Option<String> {
        self.find(action).map(|f| f(input))
    }
}

/// The labeler chain an app uses: consumer labelers first, then the defaults.
#[derive(Clone, Default)]
pub struct Labels {
    labelers: Vec<Arc<dyn ToolLabeler>>,
}

impl Labels {
    pub fn new(labelers: Vec<Arc<dyn ToolLabeler>>) -> Self {
        Labels { labelers }
    }

    pub fn call_label(&self, action: &str, input: &Value) -> String {
        self.labelers
            .iter()
            .find_map(|r| r.call(action, input))
            .or_else(|| runtime_label(action, input))
            .unwrap_or_else(|| default_label(action, input))
    }

    pub fn result_detail(&self, action: &str, ok: bool, summary: &str) -> String {
        self.labelers
            .iter()
            .find_map(|r| r.result(action, ok, summary))
            .unwrap_or_else(|| {
                let line = summary.lines().next().unwrap_or("");
                // The version is for the agent's next edit, not for the person.
                if action == ark_exec::RUN {
                    return run_result(summary);
                }
                if action.starts_with("process.") {
                    return process_result(summary);
                }
                match action.starts_with("files.") {
                    true => match line.rfind(" (version ") {
                        Some(i) => line[..i].to_string(),
                        None => line.to_string(),
                    },
                    false => line.to_string(),
                }
            })
    }

    pub fn approval_line(&self, request: &ApprovalRequest) -> String {
        self.labelers
            .iter()
            .find_map(|r| r.approval(request))
            .or_else(|| file_approval(&request.action, &request.input))
            .or_else(|| run_approval(&request.action, &request.input))
            .or_else(|| process_approval(&request.action, &request.input))
            .or_else(|| program_approval(&request.action, &request.input))
            .or_else(|| schedule_approval(&request.action, &request.input))
            .or_else(|| image_approval(&request.action, &request.input))
            .or_else(|| runtime_label(&request.action, &request.input))
            .unwrap_or_else(|| request.preview.clone())
    }

    /// A retrieval meta-tool (`search_evidence "x"`, `get_evidence inv:…`) in words.
    pub fn retrieval_label(&self, what: &str) -> String {
        if what.starts_with("search_knowledge") {
            "Consulting background knowledge"
        } else if what.starts_with("search_evidence") {
            "Searching gathered evidence"
        } else {
            "Recalling evidence"
        }
        .to_string()
    }

    /// A capability meta-tool in words.
    pub fn capability_label(&self, action: &str) -> String {
        match action {
            "find_capability" => "Looking for a capability",
            "propose_capability" => "Proposing a capability",
            _ => "Acquiring a capability",
        }
        .to_string()
    }
}

fn image_approval(action: &str, input: &Value) -> Option<String> {
    (action == "image.generate").then(|| {
        format!(
            "Generate an image\n  Prompt: {}",
            clip(
                input.get("prompt").and_then(Value::as_str).unwrap_or(""),
                240
            )
        )
    })
}

/// Arguments that usually name the thing an action works on, in priority order. Only words
/// any domain uses; an application with its own (a process, a cell, a scene) labels its
/// actions with a `ToolLabeler`.
const SUBJECT_KEYS: &[&str] = &[
    "path", "file", "name", "query", "term", "pattern", "command", "cmd", "url", "host", "domain",
    "target", "at", "artifact", "id", "text", "prompt",
];

const VERBS: &[(&[&str], &str)] = &[
    (&["read", "open", "cat", "view", "load"], "Reading"),
    (&["list", "ls", "entries", "tree"], "Listing"),
    (
        &["search", "find", "grep", "query", "lookup", "match"],
        "Searching",
    ),
    (&["save", "write", "put", "store", "append"], "Writing"),
    (
        &["edit", "update", "patch", "replace", "rewrite"],
        "Editing",
    ),
    (
        &["run", "exec", "execute", "shell", "test", "build"],
        "Running",
    ),
    (
        &["count", "wordcount", "measure", "stats", "size"],
        "Measuring",
    ),
    (
        &["fetch", "get", "http", "download", "request", "curl"],
        "Fetching",
    ),
    (
        &["info", "inspect", "describe", "show", "status", "health"],
        "Inspecting",
    ),
    (&["delete", "remove", "rm"], "Deleting"),
    (&["create", "new", "make", "init"], "Creating"),
    (&["trace", "watch", "monitor"], "Tracing"),
    (
        &["analyze", "analyse", "triage", "check", "scan"],
        "Analyzing",
    ),
    (&["compare", "diff"], "Comparing"),
    (&["summarize", "summary"], "Summarizing"),
    (&["extract"], "Extracting"),
];

/// An element as the person reads it: its name, which the tool checks against the number
/// (`“Send”`), or its number when it has no name.
fn element(input: &Value) -> String {
    let name = input
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if !name.is_empty() {
        return format!("“{}”", clip(name.trim_matches(['"', '“', '”']), 60));
    }
    match input.get("ref") {
        Some(Value::Number(n)) => format!("element {n}"),
        Some(Value::String(s)) => format!("element {}", s.trim_matches(['[', ']'])),
        _ => "an element".into(),
    }
}

/// When a schedule runs, in words, from the `agents.schedule` input.
fn when_words(input: &Value) -> String {
    let when: ark_orchestrate::When = serde_json::from_value(input.clone()).unwrap_or_default();
    match when.normalized() {
        Ok(w) => match &w.tz {
            Some(tz) => format!("{} ({tz})", w.describe()),
            None => w.describe(),
        },
        Err(_) => String::new(),
    }
}

/// Making or changing standing work, for its approval: when, and in full what every run is
/// handed (its objective and context), since nobody watches the runs. A change names the
/// schedule by what it does and when it runs now (`known`), when that is known.
fn schedule_approval(action: &str, input: &Value) -> Option<String> {
    if action != ark_orchestrate::SCHEDULE {
        return None;
    }
    Some(schedule_line(input, None))
}

/// [`schedule_approval`] for a change of `known`.
pub(crate) fn schedule_line(
    input: &Value,
    known: Option<&ark_orchestrate::ScheduleRecord>,
) -> String {
    let s = |k: &str| input.get(k).and_then(Value::as_str).unwrap_or("").trim();
    let yes = |k: &str| input.get(k).and_then(Value::as_bool) == Some(true);
    let when = when_words(input);
    let mut out = if s("schedule").is_empty() {
        format!("Run on its own {when}")
    } else {
        let mut what = Vec::new();
        if !when.is_empty() {
            what.push(format!("run it {when}"));
        }
        if !s("objective").is_empty() || input.get("context").is_some() {
            what.push("change what it does".to_string());
        }
        if yes("resume") {
            what.push("resume it".to_string());
        }
        if yes("run_now") {
            what.push("run it now".to_string());
        }
        let name = match known {
            Some(k) => format!("\"{}\" ({})", clip(&k.task.objective, 80), k.says),
            None => "a scheduled task".to_string(),
        };
        match what.is_empty() {
            true => format!("Change {name}"),
            false => format!("Change {name}: {}", what.join(", ")),
        }
    };
    if !s("to").is_empty() {
        out.push_str(&format!(", by {}", s("to")));
    }
    // In full: what runs unattended is what the person agrees to.
    for (k, head) in [("objective", "Each run"), ("context", "It is told")] {
        if !s(k).is_empty() {
            out.push_str(&format!("\n  {head}: {}", s(k).replace('\n', "\n    ")));
        }
    }
    match s("if_missed") {
        "skip" => out.push_str("\n  A run missed while the computer is off is let go."),
        "once" if !s("schedule").is_empty() => {
            out.push_str("\n  A run missed while the computer is off runs when it is back.")
        }
        _ => {}
    }
    match input.get("overlap").and_then(Value::as_bool) {
        Some(true) => out.push_str("\n  A run starts even while the last one is still going."),
        Some(false) if !s("schedule").is_empty() => {
            out.push_str("\n  A run waits for the last one to end.")
        }
        _ => {}
    }
    out
}

/// Names the schedule a change is about, from the store, so its approval says which it is.
pub(crate) struct ScheduleNames(pub ark_sdk::Ark);

impl ToolLabeler for ScheduleNames {
    fn approval(&self, request: &ApprovalRequest) -> Option<String> {
        if request.action != ark_orchestrate::SCHEDULE {
            return None;
        }
        let id = request.input.get("schedule").and_then(Value::as_str)?;
        let v = self
            .0
            .engine()
            .store
            .get_record("orch_schedule", id)
            .ok()??;
        let known: ark_orchestrate::ScheduleRecord = serde_json::from_value(v).ok()?;
        Some(schedule_line(&request.input, Some(&known)))
    }
}

/// Lines of new text an approval shows.
const PREVIEW_LINES: usize = 6;

/// What a file change will do, in words, for its approval: the file and the changes, or the
/// start of what a write saves.
fn file_approval(action: &str, input: &Value) -> Option<String> {
    let path = input
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or("a file");
    match action {
        ark_files::EDIT => {
            let edits = ark_files::Edit::parse_all(input.get("edits")?).ok()?;
            let mut out = format!("Change {path}:");
            let mut lines = Vec::new();
            for e in &edits {
                lines.push(e.describe());
                // What the edit writes, under it: cells as rows, blocks as their lines.
                match e {
                    ark_files::Edit::Set { rows, .. } | ark_files::Edit::AddSheet { rows, .. } => {
                        if rows.iter().map(Vec::len).sum::<usize>() > 1 {
                            lines.extend(
                                rows.iter()
                                    .map(|r| format!("  {}", row_line(&Value::Array(r.clone())))),
                            );
                        }
                    }
                    ark_files::Edit::Replace { text, .. }
                    | ark_files::Edit::Insert { text, .. }
                        if text.lines().filter(|l| !l.trim().is_empty()).count() > 1
                            || text.chars().count() > 60 =>
                    {
                        lines.extend(
                            text.lines()
                                .filter(|l| !l.trim().is_empty())
                                .map(|l| format!("  {l}")),
                        );
                    }
                    _ => {}
                }
            }
            for l in lines.iter().take(PREVIEW_LINES * 2) {
                let lead = &l[..l.len() - l.trim_start().len()];
                out.push_str(&format!("\n  {lead}{}", clip(l, 160)));
            }
            if lines.len() > PREVIEW_LINES * 2 {
                out.push_str(&format!("\n  … {} more", lines.len() - PREVIEW_LINES * 2));
            }
            Some(out)
        }
        ark_files::WRITE => {
            let mut out =
                format!("Save {path} in the project folder. A file with that name is replaced.");
            let lines: Vec<String> = match (
                input.get("text").and_then(Value::as_str),
                input.get("rows"),
                input.get("sheets"),
            ) {
                (Some(t), _, _) => t
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .map(str::to_string)
                    .collect(),
                (_, Some(Value::Array(rows)), _) => rows.iter().map(row_line).collect(),
                (_, _, Some(Value::Array(sheets))) => sheets
                    .iter()
                    .map(|s| {
                        let n = s.get("rows").and_then(Value::as_array).map_or(0, Vec::len);
                        format!(
                            "sheet {} ({n} rows)",
                            s.get("name").and_then(Value::as_str).unwrap_or("?")
                        )
                    })
                    .collect(),
                _ => Vec::new(),
            };
            for l in lines.iter().take(PREVIEW_LINES) {
                out.push_str(&format!("\n  {}", clip(l, 160)));
            }
            if lines.len() > PREVIEW_LINES {
                out.push_str(&format!("\n  … {} more", lines.len() - PREVIEW_LINES));
            }
            Some(out)
        }
        _ => None,
    }
}

/// A run, for its approval: what runs, where, and what it cannot do, then the program's start.
fn run_approval(action: &str, input: &Value) -> Option<String> {
    if action != ark_exec::RUN {
        return None;
    }
    let code = input.get("code").and_then(Value::as_str).unwrap_or("");
    let lines: Vec<&str> = code.lines().filter(|l| !l.trim().is_empty()).collect();
    let mut out = match purpose(input) {
        Some(p) => format!(
            "{p}\nWith a {}program in the project folder (it can change files there; no network):",
            runtime_name(input)
        ),
        None => format!(
            "Run this {}program in the project folder (it can change files there; no network):",
            runtime_name(input)
        ),
    };
    for l in lines.iter().take(PREVIEW_LINES) {
        out.push_str(&format!("\n  {}", clip(l, 160)));
    }
    if lines.len() > PREVIEW_LINES {
        out.push_str(&format!("\n  … {} more lines", lines.len() - PREVIEW_LINES));
    }
    Some(out)
}

/// What the agent says a run is for (`purpose`), as a sentence: what the person reads first,
/// before the program.
fn purpose(input: &Value) -> Option<String> {
    let p = input.get("purpose").and_then(Value::as_str)?.trim();
    let mut c = p.chars();
    let first = c.next()?;
    Some(clip(
        &(first.to_uppercase().collect::<String>() + c.as_str()),
        160,
    ))
}

/// `Python ` for `{runtime: "python"}`; nothing when the agent left the runtime to the default,
/// which only the tool knows.
fn runtime_name(input: &Value) -> String {
    let r = input.get("runtime").and_then(Value::as_str).unwrap_or("");
    let mut c = r.chars();
    c.next()
        .map(|f| f.to_uppercase().collect::<String>() + c.as_str() + " ")
        .unwrap_or_default()
}

/// A run's result in one line: the files it wrote, or how it ended.
fn run_result(summary: &str) -> String {
    let mut lines = summary.lines();
    let first = lines.next().unwrap_or("");
    match lines.next().and_then(|l| l.strip_prefix("wrote ")) {
        Some(files) if first == "exit 0" => format!("Wrote {files}"),
        Some(files) => format!("{first}; wrote {files}"),
        None if first == "exit 0" => "Done".into(),
        None => first.to_string(),
    }
}

/// A command an agent program wants to run (ADR 0043), as a process start reads: one sentence,
/// then the command, whole and as written (a person approves exactly what runs).
fn program_approval(action: &str, input: &Value) -> Option<String> {
    if action != ark_sdk::prelude::ark_runtime::worker::RUN {
        return None;
    }
    let command = input.get("command").and_then(Value::as_str).unwrap_or("");
    let lines: Vec<String> = command.lines().map(|l| format!("  {l}")).collect();
    Some(format!(
        "Run this in the project folder. Not contained: it runs as you.\n{}",
        lines.join("\n")
    ))
}

/// A process start, for its approval: the command, where, what it can reach, and the page it
/// will open.
fn process_approval(action: &str, input: &Value) -> Option<String> {
    if action != PROCESS_START {
        return None;
    }
    let s = |k: &str| input.get(k).and_then(Value::as_str).unwrap_or("").trim();
    let place = match folder(input) {
        "" => "the project folder".to_string(),
        f => format!("{f} in the project"),
    };
    // One sentence, then the command alone: a card shows the lines after the first as code.
    let then = match s("open") {
        "" => String::new(),
        o => format!(", then open {} in the browser", clip(o, 80)),
    };
    let out = format!(
        "Keep this running in {place}{then}. Not contained: it runs as you.\n  {}",
        clip(&one_line(s("command")), 200)
    );
    Some(out)
}

/// `folder` as the person reads it: no `./`, no slashes at the ends.
fn folder(input: &Value) -> &str {
    input
        .get("folder")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .trim_start_matches("./")
        .trim_matches('/')
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A process call's result in one line: `Ready at http://localhost:3000/`.
fn process_result(summary: &str) -> String {
    let first = summary.lines().next().unwrap_or("");
    let said = first.split_once(": ").map(|(_, r)| r).unwrap_or(first);
    let mut c = said.chars();
    c.next()
        .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
        .unwrap_or_default()
}

fn row_line(row: &Value) -> String {
    match row {
        Value::Array(cells) => cells
            .iter()
            .map(|c| {
                c.as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| c.to_string())
            })
            .collect::<Vec<_>>()
            .join(" | "),
        other => other.to_string(),
    }
}

/// A readable label from an action name and its input: `continuity.search {"term":"locket"}`
/// becomes `Searching "locket"`.
/// ARK's own tools that an app may offer (delegation, messages, diagrams), in words.
fn runtime_label(action: &str, input: &Value) -> Option<String> {
    let s = |k: &str| input.get(k).and_then(Value::as_str).unwrap_or("").trim();
    Some(match action {
        ark_orchestrate::DELEGATE => match s("objective") {
            "" => format!("Delegating to {}", s("to")),
            o => format!("Delegating to {}: {}", s("to"), clip(o, 60)),
        },
        ark_orchestrate::WAIT => "Waiting for delegated work".into(),
        ark_orchestrate::CREATE => format!("Creating agent {}", s("name")),
        ark_orchestrate::MESSAGE => format!("Messaging {}", s("to")),
        ark_orchestrate::START => format!("Starting a conversation with {}", s("to")),
        ark_orchestrate::SCHEDULE => match (s("schedule"), s("to")) {
            ("", "") => format!(
                "Scheduling {} {}",
                clip(s("objective"), 60),
                when_words(input)
            ),
            ("", to) => format!(
                "Scheduling {to} to {} {}",
                clip(s("objective"), 60),
                when_words(input)
            ),
            (id, _) => format!("Changing schedule {id}"),
        }
        .trim_end()
        .to_string(),
        ark_orchestrate::UNSCHEDULE => {
            if input.get("pause").and_then(Value::as_bool) == Some(true) {
                "Pausing a scheduled task".into()
            } else {
                "Stopping a scheduled task".into()
            }
        }
        ark_orchestrate::BOARD_LIST => "Reading the boards".into(),
        ark_orchestrate::BOARD_ADD => {
            let items = input.get("items").and_then(Value::as_array);
            let board = match s("board") {
                b if b.is_empty() || b.starts_with("b-") => "the board".to_string(),
                goal => format!("\"{}\"", clip(goal, 40)),
            };
            match items.map(|a| a.as_slice()) {
                Some([one]) => format!(
                    "Adding \"{}\" to {board}",
                    clip(one.get("title").and_then(Value::as_str).unwrap_or(""), 60)
                ),
                Some(many) => format!("Adding {} items to {board}", many.len()),
                None => format!("Adding to {board}"),
            }
        }
        ark_orchestrate::BOARD_UPDATE => match s("state") {
            "done" => format!("Marking {} done", s("item")),
            "doing" => format!("Moving {} to In progress", s("item")),
            "todo" => format!("Moving {} to To do", s("item")),
            _ if !s("blocked").is_empty() => format!("Marking {} blocked", s("item")),
            _ => format!("Updating {} on the board", s("item")),
        },
        // An agent program's own steps, in its own words ("Read File"); what it changed in
        // the folder, found when the task ends (ADR 0043).
        "program.files" => "Looking at what changed in the folder".into(),
        a if a.starts_with("program.") && !s("title").is_empty() => s("title").to_string(),
        "computer.apps" => "Listing the open apps".into(),
        "computer.active_window" => {
            "Reading the remembered app's window name and identifier, not its contents or controls"
                .into()
        }
        "computer.look" => format!("Looking at {}", s("app")),
        "computer.open" => format!("Opening {}", s("app")),
        "computer.click" => format!(
            "{} {} in {}",
            if input.get("right").and_then(Value::as_bool) == Some(true) {
                "Right-clicking"
            } else if input.get("double").and_then(Value::as_bool) == Some(true) {
                "Double-clicking"
            } else {
                "Clicking"
            },
            element(input),
            s("app")
        ),
        "computer.type" => format!(
            "Typing \"{}\" into {} in {}",
            clip(s("text"), 60),
            element(input),
            s("app")
        ),
        "computer.draft" => format!(
            "Drafting \"{}\" to {} in {}",
            clip(s("text"), 60),
            s("recipient"),
            s("app")
        ),
        "computer.send" => format!(
            "Sending \"{}\" to {} in {}",
            clip(s("text"), 60),
            s("recipient"),
            s("app")
        ),
        "computer.cancel" => format!("Clearing the draft to {} in {}", s("recipient"), s("app")),
        "computer.keys" => format!("Pressing {} in {}", s("keys"), s("app")),
        "computer.menu" => match s("path").trim() {
            "" => format!("Reading the menus of {}", s("app")),
            p => format!(
                "Choosing {} in {}",
                p.split(['>', '›'])
                    .map(str::trim)
                    .collect::<Vec<_>>()
                    .join(" › "),
                s("app")
            ),
        },
        "computer.see" => format!("Taking a picture of {}'s window", s("app")),
        "computer.point" => {
            let how = if input.get("right").and_then(Value::as_bool) == Some(true) {
                "Right-clicking"
            } else if input.get("double").and_then(Value::as_bool) == Some(true) {
                "Double-clicking"
            } else {
                "Clicking"
            };
            match s("what").trim() {
                "" => format!("{how} a spot in {}", s("app")),
                w => format!("{how} {} in {}", clip(w, 60), s("app")),
            }
        }
        "computer.scroll" => format!(
            "Scrolling {} in {}",
            if s("direction") == "up" { "up" } else { "down" },
            s("app")
        ),
        PROCESS_START => format!(
            "Starting `{}`{}",
            clip(&one_line(s("command")), 60),
            match folder(input) {
                "" => String::new(),
                f => format!(" in {f}"),
            }
        ),
        PROCESS_LIST => match s("id") {
            "" => "Checking the processes".into(),
            id => format!("Reading the output of {id}"),
        },
        PROCESS_STOP => format!("Stopping {}", s("id")),
        ark_exec::RUN => match (purpose(input), runtime_name(input).trim()) {
            (Some(p), _) => clip(&p, 80),
            (None, "") => "Running code".into(),
            (None, r) => format!("Running {r}"),
        },
        "diagram.create" => match s("title") {
            "" => "Drawing a diagram".into(),
            t => format!("Drawing \"{}\"", clip(t, 60)),
        },
        "diagram.read" => "Reading a diagram".into(),
        ark_files::LIST => match s("path").trim_matches(['.', '/']) {
            "" => "Looking in the project folder".into(),
            p => format!("Looking in {p}"),
        },
        ark_files::READ => match (s("sheet"), s("range")) {
            ("", "") => format!("Reading {}", s("path")),
            (sheet, "") => format!("Reading {} › {sheet}", s("path")),
            ("", range) => format!("Reading {} {range}", s("path")),
            (sheet, range) => format!("Reading {} › {sheet} {range}", s("path")),
        },
        ark_files::SEARCH => format!("Searching for \"{}\"", clip(s("query"), 60)),
        ark_files::WRITE => format!("Writing {}", s("path")),
        ark_files::EDIT => format!("Editing {}", s("path")),
        _ => return None,
    })
}

/// A turn's error as both front-ends show it. A reply the model could not fix (ADR 0050)
/// reads in the person's words; its detail stays in the turn's record for the model.
pub fn person_error(error: &str) -> String {
    if error.starts_with(ark_sdk::prelude::ark_runtime::UNUSABLE_REPLY) {
        "The model's reply could not be used.".to_string()
    } else {
        error.to_string()
    }
}

pub fn default_label(action: &str, input: &Value) -> String {
    let (ns, verb) = action.split_once('.').unwrap_or((action, action));
    let verb_l = verb.to_ascii_lowercase();
    let gerund = VERBS.iter().find_map(|(words, g)| {
        words
            .iter()
            .any(|w| verb_l == *w || verb_l.starts_with(w) || (w.len() >= 4 && verb_l.ends_with(w)))
            .then_some(*g)
    });
    let subject = subject(input);
    match (gerund, subject) {
        (Some(g), Some((key, s))) => {
            if matches!(key, "query" | "term" | "pattern") {
                format!("{g} \"{s}\"")
            } else {
                format!("{g} {s}")
            }
        }
        (Some(g), None) => format!("{g} {}", ns.replace(['_', '-'], " ")),
        (None, Some((_, s))) => format!("{} · {s}", humanize(action)),
        (None, None) => humanize(action),
    }
}

fn subject(input: &Value) -> Option<(&'static str, String)> {
    let obj = input.as_object()?;
    for key in SUBJECT_KEYS {
        if let Some(v) = obj.get(*key) {
            let s = match v {
                Value::String(s) if !s.trim().is_empty() => s.clone(),
                Value::Number(n) => n.to_string(),
                _ => continue,
            };
            return Some((key, clip(&s, 60)));
        }
    }
    None
}

fn humanize(action: &str) -> String {
    let s = action.replace(['.', '_'], " ");
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn image_generation_approval_exposes_the_prompt() {
        let line = Labels::new(Vec::new()).approval_line(&ApprovalRequest {
            invocation: None,
            project: None,
            action: "image.generate".into(),
            summary: "generate one image".into(),
            risk: ark_sdk::prelude::RiskTier::Mutate,
            input: json!({"prompt": "a blue square on white"}),
            class: "image.generate".into(),
            preview: "image.generate(…)".into(),
            agent: None,
            task: None,
            ask_each_time: true,
            cut_short: false,
        });
        assert!(line.contains("a blue square on white"), "{line}");
    }

    #[test]
    fn a_file_change_asks_with_what_it_writes() {
        let l = Labels::new(Vec::new());
        let req = ApprovalRequest {
            invocation: None,
            project: None,
            action: "files.edit".into(),
            summary: String::new(),
            risk: ark_sdk::prelude::RiskTier::Mutate,
            input: json!({ "path": "sales.xlsx", "edits": [
                { "cell": "B4", "value": 150 },
                { "add_sheet": "Summary", "rows": [["Total", "=SUM(Sales!B2:B9)"], ["Best", "North"]] }
            ] }),
            class: "files.edit".into(),
            preview: String::new(),
            agent: None,
            task: None,
            ask_each_time: false,
            cut_short: false,
        };
        assert_eq!(
            l.approval_line(&req),
            "Change sales.xlsx:\n  B4 = 150\n  add sheet Summary (2 rows)\n    Total | =SUM(Sales!B2:B9)\n    Best | North"
        );
        assert_eq!(
            l.call_label("files.read", &json!({"path": "sales.xlsx", "sheet": "Q1"})),
            "Reading sales.xlsx › Q1"
        );
        assert_eq!(
            l.result_detail(
                "files.edit",
                true,
                "saved sales.xlsx: B4 = 150 (version 1f2e)"
            ),
            "saved sales.xlsx: B4 = 150"
        );
    }

    #[test]
    fn a_run_asks_with_its_program_and_says_what_it_wrote() {
        let l = Labels::new(Vec::new());
        let code = "import pandas as pd\n\ndf = pd.read_excel('sales.xlsx')\nprint(df.sum())";
        let req = ApprovalRequest {
            invocation: None,
            project: None,
            action: ark_exec::RUN.into(),
            summary: String::new(),
            risk: ark_sdk::prelude::RiskTier::Mutate,
            input: json!({ "code": code, "runtime": "python" }),
            class: ark_exec::RUN.into(),
            preview: String::new(),
            agent: None,
            task: None,
            ask_each_time: false,
            cut_short: false,
        };
        assert_eq!(
            l.approval_line(&req),
            "Run this Python program in the project folder (it can change files there; no network):\n  import pandas as pd\n  df = pd.read_excel('sales.xlsx')\n  print(df.sum())"
        );
        assert_eq!(
            l.call_label(ark_exec::RUN, &json!({ "code": code, "runtime": "python" })),
            "Running Python"
        );
        assert_eq!(
            l.call_label(ark_exec::RUN, &json!({ "code": code })),
            "Running code"
        );
        let said =
            json!({ "code": code, "runtime": "python", "purpose": "total the sales per region" });
        assert_eq!(
            l.call_label(ark_exec::RUN, &said),
            "Total the sales per region"
        );
        assert_eq!(
            l.approval_line(&ApprovalRequest {
                input: said,
                ..req.clone()
            }),
            "Total the sales per region\nWith a Python program in the project folder (it can change files there; no network):\n  import pandas as pd\n  df = pd.read_excel('sales.xlsx')\n  print(df.sum())"
        );
        let done = "exit 0\nwrote chart.png, summary.csv\nstdout:\n12";
        assert_eq!(
            l.result_detail(ark_exec::RUN, true, done),
            "Wrote chart.png, summary.csv"
        );
        assert_eq!(
            l.result_detail(ark_exec::RUN, true, "exit 1\nstderr:\nKeyError"),
            "exit 1"
        );
        assert_eq!(
            l.result_detail(ark_exec::RUN, true, "exit 0\nstdout:\n12"),
            "Done"
        );
    }

    #[test]
    fn a_process_start_asks_with_its_command_and_folder() {
        let l = Labels::new(vec![]);
        let input = json!({ "command": "npm  run dev", "folder": "./web/", "open": "/home" });
        let req = ApprovalRequest {
            invocation: None,
            project: None,
            action: "process.start".into(),
            summary: String::new(),
            risk: ark_sdk::prelude::RiskTier::Mutate,
            input: input.clone(),
            class: "process.start:npm run dev@./web/".into(),
            preview: String::new(),
            agent: None,
            task: None,
            ask_each_time: false,
            cut_short: false,
        };
        assert_eq!(
            l.approval_line(&req),
            "Keep this running in web in the project, then open /home in the browser. Not contained: it runs as you.\n  npm run dev"
        );
        assert_eq!(
            l.call_label("process.start", &input),
            "Starting `npm run dev` in web"
        );
        assert_eq!(
            l.result_detail(
                "process.start",
                true,
                "p1 npm run dev (in web): ready at http://localhost:3000/\noutput:\n..."
            ),
            "Ready at http://localhost:3000/"
        );
        assert_eq!(
            l.call_label("process.list", &json!({ "id": "p1" })),
            "Reading the output of p1"
        );
        assert_eq!(
            l.call_label("process.stop", &json!({ "id": "p1" })),
            "Stopping p1"
        );
    }

    #[test]
    fn a_schedule_approval_shows_when_and_all_each_run_is_handed() {
        let l = Labels::new(Vec::new());
        let req = |input: Value| ApprovalRequest {
            invocation: None,
            project: None,
            action: ark_orchestrate::SCHEDULE.into(),
            summary: String::new(),
            risk: ark_sdk::prelude::RiskTier::Mutate,
            input,
            class: ark_orchestrate::SCHEDULE.into(),
            preview: String::new(),
            agent: None,
            task: None,
            ask_each_time: true,
            cut_short: false,
        };
        assert_eq!(
            l.approval_line(&req(json!({
                "objective": "Recount the stock in stock.xlsx, update the counts and summarize the changes",
                "context": "The counts are on the sheet Inventory.",
                "at": "02:00", "tz": "Europe/Madrid"
            }))),
            "Run on its own every day at 02:00 (Europe/Madrid)\n  Each run: Recount the stock in stock.xlsx, update the counts and summarize the changes\n  It is told: The counts are on the sheet Inventory."
        );
        assert_eq!(
            l.approval_line(&req(
                json!({ "schedule": "s-1a2b3c4d", "at": "03:00", "resume": true })
            )),
            "Change a scheduled task: run it every day at 03:00, resume it"
        );
        // Long instructions are shown whole, never cut: they run with nobody watching.
        let long = format!(
            "Count the stock. {}",
            "Then check the shelves again. ".repeat(60)
        );
        let line = l.approval_line(&req(
            json!({ "objective": "Count", "context": long.trim(), "at": "02:00" }),
        ));
        assert!(line.ends_with("Then check the shelves again."), "{line}");
        assert!(line.len() > 1800);
        let known: ark_orchestrate::ScheduleRecord = serde_json::from_value(json!({
            "id": "s-1", "project": "p", "agent": "a", "task": { "objective": "Recount the stock" },
            "when": { "at": "02:00" }, "says": "every day at 02:00", "created_ms": 0
        }))
        .unwrap();
        assert_eq!(
            schedule_line(&json!({ "schedule": "s-1", "run_now": true }), Some(&known)),
            "Change \"Recount the stock\" (every day at 02:00): run it now"
        );
        assert_eq!(
            l.call_label(
                ark_orchestrate::UNSCHEDULE,
                &json!({ "schedule": "s-1", "pause": true })
            ),
            "Pausing a scheduled task"
        );
    }

    #[test]
    fn scheduling_and_the_computer_read_naturally() {
        let l = Labels::new(Vec::new());
        assert_eq!(
            l.approval_line(&ApprovalRequest {
                invocation: None,
                project: None,
                action: "computer.active_window".into(),
                summary: String::new(),
                risk: ark_sdk::prelude::RiskTier::Mutate,
                input: json!({}),
                class: "computer.active_window".into(),
                preview: String::new(),
                agent: None,
                task: None,
                ask_each_time: true,
                cut_short: false,
            }),
            "Reading the remembered app's window name and identifier, not its contents or controls"
        );
        assert_eq!(
            l.call_label(
                "agents.schedule",
                &json!({ "to": "news", "objective": "summarize the news", "at": "8:30", "days": ["weekdays"] })
            ),
            "Scheduling news to summarize the news every weekday at 08:30"
        );
        assert_eq!(
            l.call_label(
                "computer.type",
                &json!({ "app": "Notes", "ref": 3, "name": "Title", "text": "Plan", "submit": true })
            ),
            "Typing \"Plan\" into “Title” in Notes"
        );
        assert_eq!(
            l.call_label(
                "computer.send",
                &json!({ "app": "Messages", "recipient": "Lara", "text": "Hola" })
            ),
            "Sending \"Hola\" to Lara in Messages"
        );
        assert_eq!(
            l.call_label(
                "computer.click",
                &json!({ "app": "Mail", "ref": "[2]", "name": "Send" })
            ),
            "Clicking “Send” in Mail"
        );
        assert_eq!(
            l.call_label(
                "computer.click",
                &json!({ "app": "Mail", "ref": 2, "name": "" })
            ),
            "Clicking element 2 in Mail"
        );
        assert_eq!(
            l.call_label(
                "computer.click",
                &json!({ "app": "Finder", "ref": 4, "name": "report.pdf", "double": true })
            ),
            "Double-clicking “report.pdf” in Finder"
        );
        assert_eq!(
            l.call_label(
                "computer.menu",
                &json!({ "app": "Pages", "path": "File > Export To›PDF…" })
            ),
            "Choosing File › Export To › PDF… in Pages"
        );
        assert_eq!(
            l.call_label("computer.menu", &json!({ "app": "Pages" })),
            "Reading the menus of Pages"
        );
        assert_eq!(
            l.call_label(
                "computer.scroll",
                &json!({ "app": "WhatsApp", "direction": "up" })
            ),
            "Scrolling up in WhatsApp"
        );
        assert_eq!(
            l.call_label("computer.see", &json!({ "app": "Spotify" })),
            "Taking a picture of Spotify's window"
        );
        assert_eq!(
            l.call_label(
                "computer.point",
                &json!({ "app": "Spotify", "x": 640, "y": 788, "what": "the play button" })
            ),
            "Clicking the play button in Spotify"
        );
    }

    #[test]
    fn runtime_tools_read_naturally() {
        let l = Labels::new(Vec::new());
        assert_eq!(
            l.call_label(
                "agents.delegate",
                &json!({ "to": "reader-a", "objective": "read the spring review" })
            ),
            "Delegating to reader-a: read the spring review"
        );
        assert_eq!(
            l.call_label("agents.message", &json!({ "to": "lead", "text": "x" })),
            "Messaging lead"
        );

        assert_eq!(
            l.call_label(
                "agents.start",
                &json!({ "to": "reviewer", "objective": "Review" })
            ),
            "Starting a conversation with reviewer"
        );
        assert_eq!(
            l.call_label("diagram.create", &json!({ "title": "Themes" })),
            "Drawing \"Themes\""
        );
    }

    #[test]
    fn default_labels_read_naturally() {
        assert_eq!(
            default_label("manuscript.read", &json!({ "file": "chapter-04.md" })),
            "Reading chapter-04.md"
        );
        assert_eq!(
            default_label("continuity.search", &json!({ "term": "locket" })),
            "Searching \"locket\""
        );
        assert_eq!(
            default_label("manuscript.list", &json!({})),
            "Listing manuscript"
        );
        assert_eq!(
            default_label("code.exec", &json!({ "command": "cargo test" })),
            "Running cargo test"
        );
        assert_eq!(
            default_label("manuscript.wordcount", &json!({ "file": "ch1.md" })),
            "Measuring ch1.md"
        );
        assert_eq!(
            default_label("calc.eval", &json!({ "expr": "6*7" })),
            "Calc eval"
        );
    }

    #[test]
    fn consumer_labels_win_over_defaults() {
        let labels = ActivityLabels::new()
            .label("manuscript.save", |i| {
                format!("Saving {}", i["file"].as_str().unwrap_or("?"))
            })
            .namespace("notes", |_| "Working in the notes".into());
        let a = Labels::new(vec![Arc::new(labels)]);
        assert_eq!(
            a.call_label("manuscript.save", &json!({ "file": "ch4.md" })),
            "Saving ch4.md"
        );
        assert_eq!(
            a.call_label("notes.tidy", &json!({})),
            "Working in the notes"
        );
        assert_eq!(
            a.call_label("manuscript.read", &json!({ "file": "x" })),
            "Reading x"
        );
    }
}
