//! Agents on other agent programs, over the Agent Client Protocol (ADR 0039, ADR 0043).
//!
//! ```text
//! task (ARK) ─ brief, folder ─► the program (Claude Code, Codex) with its own tools
//!                                 │ wants to change a file / run a command
//!            ◄── permission ──────┘ an ARK question on the task (kept, any client answers)
//!            ── allow once / reject once ─►
//!            ◄── its answer, its usage ── the task's answer; its files come from the folder
//! ```
//!
//! ARK stays the authority: the program starts with a clean environment, without the person's
//! or the project's own allow rules, in the mode that asks; every grant is ARK's and names the
//! file or the command. Which programs exist and how each is started is data ([`Catalog`]).

mod client;

use std::path::PathBuf;
use std::sync::Arc;

use ark_runtime::worker::{Worker, Workers};

pub use client::Acp;

/// One agent program ARK can start.
#[derive(Clone, Debug)]
pub struct Entry {
    /// Its name in a spec (`agent:claude-code`).
    pub name: &'static str,
    /// What a person calls it.
    pub title: &'static str,
    /// The program that speaks the protocol, found on `PATH`.
    pub program: &'static str,
    /// An environment variable that names the program's path instead.
    pub path_var: &'static str,
    /// Arguments that keep it inside ARK's answers (its own approval policy and sandbox).
    pub args: &'static [&'static str],
    /// How a job's model reaches it.
    pub model: ModelSetting,
    /// The model when the spec names none: ARK sets one per job and never takes the one in
    /// the person's own settings (the adapters lag their programs and refuse newer names).
    /// `None`: the program's own default.
    pub default_model: Option<&'static str>,
    /// `_meta` for a new or loaded session: settings it must not read (allow rules, hooks).
    pub session_meta: fn() -> serde_json::Value,
    /// The mode it must be in, when it has modes: the one that asks before a change.
    pub mode: Option<&'static str>,
    /// The adapter reads the person's settings from their home, or from the folder this
    /// variable names. It gets an empty home and not the variable, so the person's own mode
    /// and rules never apply; the program it starts gets both, as set, through the session
    /// (its login and its sessions stay the person's).
    pub settings_dir: Option<&'static str>,
    /// Environment variables it may see besides the base ones (its own login's location).
    pub env: &'static [&'static str],
    /// How a person sets it up when it is missing.
    pub setup: &'static str,
}

/// How a job's model reaches a program.
#[derive(Clone, Copy, Debug)]
pub enum ModelSetting {
    /// In the session's `_meta.claudeCode.options.model`.
    ClaudeMeta,
    /// As `-c model="<model>"`.
    ConfigArg,
}

/// The variables every program sees: who and where the person is, and nothing of ARK's own
/// (keys, tokens, secrets).
pub const BASE_ENV: &[&str] = &[
    "HOME", "PATH", "USER", "LOGNAME", "SHELL", "TMPDIR", "LANG", "LC_ALL", "LC_CTYPE", "TERM",
];

/// Claude Code without the person's or the project's settings: no allow rules, hooks or
/// servers from them, so every change asks ARK.
fn claude_meta() -> serde_json::Value {
    serde_json::json!({ "claudeCode": { "options": { "settingSources": [] } } })
}

fn no_meta() -> serde_json::Value {
    serde_json::Value::Null
}

/// The programs ARK knows.
pub fn entries() -> Vec<Entry> {
    vec![
        Entry {
            name: "claude-code",
            title: "Claude Code",
            program: "claude-agent-acp",
            path_var: "ARK_CLAUDE_CODE_ACP",
            args: &[],
            model: ModelSetting::ClaudeMeta,
            default_model: None,
            session_meta: claude_meta,
            mode: Some("default"),
            settings_dir: Some("CLAUDE_CONFIG_DIR"),
            env: &[],
            setup: "install Claude Code and its protocol adapter (`npm install -g @zed-industries/claude-agent-acp`), then sign in with `claude`",
        },
        Entry {
            name: "codex",
            title: "Codex",
            program: "codex-acp",
            path_var: "ARK_CODEX_ACP",
            // Every command it is not sure of asks; nothing writes without a question.
            args: &[
                "-c",
                "approval_policy=\"untrusted\"",
                "-c",
                "sandbox_mode=\"read-only\"",
            ],
            model: ModelSetting::ConfigArg,
            default_model: Some("gpt-5.5"),
            session_meta: no_meta,
            mode: None,
            settings_dir: None,
            env: &["CODEX_HOME"],
            setup: "install Codex and its protocol adapter (`npm install -g @zed-industries/codex-acp`), then sign in with `codex`",
        },
    ]
}

/// The programs of [`entries`] that can run here.
#[derive(Clone)]
pub struct Catalog {
    entries: Vec<Entry>,
}

impl Catalog {
    /// Every known program, found when asked for.
    pub fn installed() -> Catalog {
        Catalog { entries: entries() }
    }

    /// Only these entries (tests, an app's own programs).
    pub fn of(entries: Vec<Entry>) -> Catalog {
        Catalog { entries }
    }

    /// The path of an entry's program: its variable, else `PATH`.
    pub fn find(e: &Entry) -> Option<PathBuf> {
        if let Some(p) = std::env::var_os(e.path_var).map(PathBuf::from) {
            return p.is_file().then_some(p);
        }
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .map(|d| d.join(e.program))
            .find(|p| p.is_file())
    }
}

impl Workers for Catalog {
    fn worker(&self, name: &str) -> Option<Result<Arc<dyn Worker>, String>> {
        let e = self.entries.iter().find(|e| e.name == name)?;
        Some(match Catalog::find(e) {
            Some(program) => Ok(Arc::new(Acp::new(e.clone(), program)) as Arc<dyn Worker>),
            None => Err(format!("{} is not set up here: {}", e.title, e.setup)),
        })
    }
}
