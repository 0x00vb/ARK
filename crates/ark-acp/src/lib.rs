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
//! file or the command. On macOS it runs contained (ADR 0074, [`Contain`]): it writes only the
//! project and its own state. Which programs exist and how each is started is data ([`Catalog`]).

mod client;
mod guard;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ark_runtime::worker::{
    Worker, WorkerAction, WorkerModel, WorkerOffer, WorkerSetup, WorkerState, Workers,
};

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
    /// The provider's own CLI. It differs from `program` for connector-based workers.
    pub provider_program: &'static str,
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
    /// The versions of the program's connector ARK was checked with (prefixes: `0.23.`). How
    /// it is kept inside ARK's answers depends on the connector, so another version is not
    /// used. Empty: any.
    pub checked: &'static [&'static str],
    /// The adapter reads the person's settings from their home, or from the folder this
    /// variable names. It gets an empty home and not the variable, so the person's own mode
    /// and rules never apply; the program it starts gets both, as set, through the session
    /// (its login and its sessions stay the person's).
    pub settings_dir: Option<&'static str>,
    /// Environment variables it may see besides the base ones (its own login's location).
    pub env: &'static [&'static str],
    /// A private home owned by this catalog. The provider writes its login there itself.
    pub isolated_home: Option<PathBuf>,
    /// Models offered for this worker. Empty uses the program's default only.
    pub models: &'static [(&'static str, &'static str)],
    /// How a person sets it up when it is missing.
    pub setup: &'static str,
    /// How ARK contains it where the system can (ADR 0074). `None`: ARK does not.
    pub contain: Option<Contain>,
}

/// What a contained program may touch besides its project (ADR 0074). It reads what is outside
/// the person's home; in the home, only its program, its own folders and these.
#[derive(Clone, Debug, Default)]
pub struct Contain {
    /// Folders it writes: its own state.
    pub folders: Vec<PathBuf>,
    /// Files it writes, with their siblings named after them and a dot (a lock, a copy).
    pub files: Vec<PathBuf>,
    /// Names inside each of `folders` that stay read-only: what runs later outside the
    /// sandbox (its own program, settings with hooks, its ask rules).
    pub kept: &'static [&'static str],
    /// Its login is in the macOS keychain.
    pub keychain: bool,
    /// Keys of its JSON state files that ARK puts back after each run.
    pub guarded: Vec<Guard>,
}

/// Keys of a JSON file a contained program may write but not change: what would start a
/// program, or allow a tool, in its next run outside the sandbox. Each key is a JSON pointer;
/// `*` stands for any key of an object. After each run ARK puts back their values from before
/// it, and removes what it added under them; the rest of the file stays as the program left it.
#[derive(Clone, Debug)]
pub struct Guard {
    pub file: PathBuf,
    pub keys: &'static [&'static str],
}

/// How a job's model reaches a program.
#[derive(Clone, Copy, Debug)]
pub enum ModelSetting {
    /// In the session's `_meta.claudeCode.options.model`.
    ClaudeMeta,
    /// As `-c model="<model>"`.
    ConfigArg,
    /// ACP `session/set_config_option` with config id `model`.
    ConfigOption,
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

const GROK_CONFIG: &str = r#"[cli]
auto_update = false

[ui]
permission_mode = "ask"
remember_tool_approvals = false

[permission]
ask = ["Bash(*)", "Edit(*)"]

[compat.claude]
skills = false
rules = false
agents = false
mcps = false
hooks = false
sessions = false

[compat.cursor]
skills = false
rules = false
agents = false
mcps = false
hooks = false
sessions = false

[compat.codex]
sessions = false

[workflows]
enabled = false
"#;

/// The programs ARK knows.
pub fn entries() -> Vec<Entry> {
    let root = ark_os::dirs::app_home(".ark", "ARK")
        .unwrap_or_else(std::env::temp_dir)
        .join("workers");
    entries_at(Some(&root))
}

/// What Claude Code writes in `~/.claude` as it works: transcripts, todos, history, caches.
/// Nothing else there is writable: settings (hooks, allow rules), commands, agents, skills,
/// plugins and backups are what its next run outside the sandbox reads and runs.
const CLAUDE_FOLDERS: &[&str] = &[
    "projects",
    "todos",
    "statsig",
    "session-env",
    "file-history",
    "plans",
    "debug",
    "telemetry",
    "cache",
    "paste-cache",
    "ide",
    "sessions",
];
const CLAUDE_FILES: &[&str] = &["history.jsonl", "stats-cache.json", ".credentials.json"];

/// What in `~/.claude.json` would start a program, or allow a tool, in Claude Code's next run
/// outside the sandbox: put back after each contained run.
const CLAUDE_GUARDED: &[&str] = &[
    "/mcpServers",
    "/projects/*/mcpServers",
    "/projects/*/enabledMcpjsonServers",
    "/projects/*/enableAllProjectMcpServers",
    "/projects/*/allowedTools",
    "/projects/*/hasTrustDialogAccepted",
];

fn entries_at(root: Option<&Path>) -> Vec<Entry> {
    let home = ark_os::dirs::home();
    let home = home.as_deref();
    vec![
        Entry {
            name: "claude-code",
            title: "Claude Code",
            program: "claude-agent-acp",
            provider_program: "claude",
            path_var: "ARK_CLAUDE_CODE_ACP",
            args: &[],
            model: ModelSetting::ClaudeMeta,
            default_model: None,
            session_meta: claude_meta,
            mode: Some("default"),
            checked: &["0.23."],
            settings_dir: Some("CLAUDE_CONFIG_DIR"),
            env: &[],
            isolated_home: None,
            models: &[("sonnet", "Sonnet"), ("opus", "Opus"), ("haiku", "Haiku")],
            setup: "install Claude Code and its connector (`npm install -g @zed-industries/claude-agent-acp@0.23`), then sign in with `claude`",
            contain: home.map(|h| Contain {
                folders: CLAUDE_FOLDERS
                    .iter()
                    .map(|f| h.join(".claude").join(f))
                    .chain([if cfg!(target_os = "macos") {
                        h.join("Library/Caches/claude-cli-nodejs")
                    } else {
                        h.join(".cache/claude-cli-nodejs")
                    }])
                    .collect(),
                files: CLAUDE_FILES
                    .iter()
                    .map(|f| h.join(".claude").join(f))
                    .chain([h.join(".claude.json")])
                    .collect(),
                kept: &[],
                keychain: true,
                guarded: vec![Guard {
                    file: h.join(".claude.json"),
                    keys: CLAUDE_GUARDED,
                }],
            }),
        },
        Entry {
            name: "codex",
            title: "Codex",
            program: "codex-acp",
            provider_program: "codex",
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
            checked: &["0.16."],
            settings_dir: None,
            env: &["CODEX_HOME"],
            isolated_home: None,
            models: &[],
            setup: "install Codex and its connector (`npm install -g @zed-industries/codex-acp@0.16`), then sign in with `codex`",
            // Codex runs each command in its own sandbox, and macOS cannot start one sandbox
            // inside another (`sandbox_apply: Operation not permitted`).
            contain: None,
        },
        Entry {
            name: "grok",
            title: "Grok",
            program: "grok",
            provider_program: "grok",
            path_var: "ARK_GROK_ACP",
            args: &["agent", "stdio"],
            model: ModelSetting::ConfigOption,
            default_model: None,
            session_meta: no_meta,
            mode: None,
            checked: &["1.0."],
            settings_dir: None,
            env: &[],
            isolated_home: root.map(|r| r.join("grok")),
            models: &[
                ("grok-4.7", "Grok 4.7"),
                ("grok-4.7-build-fast", "Grok 4.7 Fast"),
                ("grok-4.6", "Grok 4.6"),
                ("grok-4.5", "Grok 4.5"),
            ],
            setup: "install Grok 1.0.50 in ARK's private worker folder, then sign in with `grok login --device-auth`",
            contain: root.map(|r| Contain {
                folders: vec![r.join("grok")],
                files: Vec::new(),
                // What ARK's own setup and probes run or read outside the sandbox: its
                // program, its ask rules, and npm's settings and cache for an update.
                kept: &[
                    "node_modules",
                    "config.toml",
                    ".npmrc",
                    "package.json",
                    "package-lock.json",
                    ".npm",
                ],
                keychain: false,
                guarded: Vec::new(),
            }),
        },
    ]
}

/// The programs of [`entries`] that can run here.
#[derive(Clone)]
pub struct Catalog {
    entries: Vec<Entry>,
    /// Folders a contained program never reads: ARK's store (ADR 0074).
    hidden: Vec<PathBuf>,
}

#[derive(Debug)]
enum ProbeError {
    Failed,
    TimedOut,
}

impl Catalog {
    /// Every known program, found when asked for.
    pub fn installed() -> Catalog {
        Catalog {
            entries: entries(),
            hidden: ark_os::dirs::app_home(".ark", "ARK").into_iter().collect(),
        }
    }

    /// The built-in catalog with private worker state under `root`.
    pub fn at(root: impl Into<PathBuf>) -> Catalog {
        let root = root.into();
        Catalog {
            entries: entries_at(Some(&root)),
            hidden: Vec::new(),
        }
    }

    /// Only these entries (tests, an app's own programs).
    pub fn of(entries: Vec<Entry>) -> Catalog {
        Catalog {
            entries,
            hidden: Vec::new(),
        }
    }

    /// Contained programs never read `folders` (ARK's store: other projects, keys), except
    /// their own state inside them (ADR 0074).
    pub fn hiding(mut self, folders: Vec<PathBuf>) -> Catalog {
        self.hidden = folders;
        self
    }

    /// The path of an entry's program: its variable, else `PATH`.
    pub fn find(e: &Entry) -> Option<PathBuf> {
        if let Some(p) = std::env::var_os(e.path_var).map(PathBuf::from) {
            return p.is_file().then_some(p);
        }
        if let Some(home) = &e.isolated_home {
            let local = home.join("node_modules").join(".bin").join(format!(
                "{}{}",
                e.program,
                std::env::consts::EXE_SUFFIX
            ));
            if local.is_file() {
                return Some(local);
            }
        }
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .map(|d| d.join(e.program))
            .find(|p| p.is_file())
    }

    fn find_named(program: &str) -> Option<PathBuf> {
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .map(|d| d.join(format!("{program}{}", std::env::consts::EXE_SUFFIX)))
            .find(|p| p.is_file())
    }

    fn prepare(e: &Entry) -> Result<(), String> {
        let Some(home) = &e.isolated_home else {
            return Ok(());
        };
        std::fs::create_dir_all(home)
            .map_err(|error| format!("could not prepare {}: {error}", e.title))?;
        let config = home.join("config.toml");
        if std::fs::read_to_string(&config).ok().as_deref() != Some(GROK_CONFIG) {
            std::fs::write(&config, GROK_CONFIG)
                .map_err(|error| format!("could not prepare {}: {error}", e.title))?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(home, std::fs::Permissions::from_mode(0o700));
            let _ = std::fs::set_permissions(config, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }

    fn output(
        program: &Path,
        args: &[&str],
        e: &Entry,
    ) -> Result<std::process::Output, ProbeError> {
        let mut command = Command::new(program);
        command
            .args(args)
            .env_clear()
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for name in BASE_ENV {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        if let Some(home) = &e.isolated_home {
            command
                .env("HOME", home)
                .env("GROK_HOME", home)
                .env("GROK_AGENT_DASHBOARD", "0")
                .env("GROK_WORKFLOWS", "0");
        }
        let mut child = command.spawn().map_err(|_| ProbeError::Failed)?;
        let started = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_)) => return child.wait_with_output().map_err(|_| ProbeError::Failed),
                Ok(None) if started.elapsed() < Duration::from_secs(3) => {
                    std::thread::sleep(Duration::from_millis(25));
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(ProbeError::TimedOut);
                }
            }
        }
    }

    fn version(e: &Entry, program: &Path) -> Result<Option<String>, ProbeError> {
        let output = Self::output(program, &["--version"], e)?;
        let text = if output.stdout.is_empty() {
            &output.stderr
        } else {
            &output.stdout
        };
        Ok(String::from_utf8_lossy(text)
            .split_whitespace()
            .find(|word| word.chars().next().is_some_and(|c| c.is_ascii_digit()))
            .map(|word| {
                word.trim_matches(|c: char| !c.is_ascii_digit() && c != '.')
                    .to_string()
            }))
    }

    fn signed_in(e: &Entry, provider: &Path) -> Result<bool, ProbeError> {
        match e.name {
            "claude-code" => Ok(Self::output(provider, &["auth", "status"], e)?
                .status
                .success()),
            "codex" => Ok(Self::output(provider, &["login", "status"], e)?
                .status
                .success()),
            "grok" => Ok(e
                .isolated_home
                .as_ref()
                .is_some_and(|home| home.join("auth.json").is_file())),
            _ => Ok(true),
        }
    }

    fn action(e: &Entry, kind: &str, label: impl Into<String>) -> WorkerAction {
        WorkerAction {
            id: format!("worker-setup-v1:{}:{kind}", e.name),
            kind: kind.to_string(),
            label: label.into(),
        }
    }

    fn offer(e: &Entry) -> WorkerOffer {
        if cfg!(windows) && e.name == "grok" {
            return WorkerOffer {
                spec: format!("agent:{}", e.name),
                title: e.title.to_string(),
                discoverable: true,
                state: WorkerState::Unsupported,
                models: e
                    .models
                    .iter()
                    .map(|(model, title)| WorkerModel {
                        spec: format!("agent:{}:{model}", e.name),
                        title: title.to_string(),
                    })
                    .collect(),
                action: None,
            };
        }
        let connector = Self::find(e);
        let provider = if e.provider_program == e.program {
            connector.clone()
        } else {
            Self::find_named(e.provider_program)
        };
        let timed_out = || {
            (
                WorkerState::TimedOut,
                Some(Self::action(e, "check", "Check again")),
            )
        };
        let (state, action) = match (provider.as_deref(), connector.as_deref()) {
            (None, _) => (
                WorkerState::Missing,
                Some(Self::action(e, "install", format!("Install {}", e.title))),
            ),
            (Some(_), None) => (
                WorkerState::ConnectorMissing,
                Some(Self::action(e, "install_connector", "Install connector")),
            ),
            (Some(provider), Some(connector)) => match Self::version(e, connector) {
                Err(ProbeError::TimedOut) => timed_out(),
                Ok(Some(version))
                    if !e.checked.is_empty()
                        && !e.checked.iter().any(|want| version.starts_with(want)) =>
                {
                    (
                        WorkerState::NeedsUpdate,
                        Some(Self::action(e, "update", format!("Update {}", e.title))),
                    )
                }
                Ok(None) | Err(ProbeError::Failed) if !e.checked.is_empty() => (
                    WorkerState::NeedsUpdate,
                    Some(Self::action(e, "update", format!("Update {}", e.title))),
                ),
                _ => match Self::signed_in(e, provider) {
                    Err(ProbeError::TimedOut) => timed_out(),
                    Ok(true) => (WorkerState::Ready, None),
                    _ => (
                        WorkerState::SignedOut,
                        Some(Self::action(
                            e,
                            "sign_in",
                            format!("Sign in to {}", e.title),
                        )),
                    ),
                },
            },
        };
        WorkerOffer {
            spec: format!("agent:{}", e.name),
            title: e.title.to_string(),
            discoverable: true,
            state,
            models: e
                .models
                .iter()
                .map(|(model, title)| WorkerModel {
                    spec: format!("agent:{}:{model}", e.name),
                    title: title.to_string(),
                })
                .collect(),
            action,
        }
    }

    fn compatibility_offer(e: &Entry) -> WorkerOffer {
        WorkerOffer {
            spec: format!("agent:{}", e.name),
            title: e.title.to_string(),
            discoverable: false,
            state: if Self::find(e).is_some() {
                WorkerState::Ready
            } else {
                WorkerState::Unsupported
            },
            models: e
                .models
                .iter()
                .map(|(model, title)| WorkerModel {
                    spec: format!("agent:{}:{model}", e.name),
                    title: title.to_string(),
                })
                .collect(),
            action: None,
        }
    }

    fn shell(line: String) -> Vec<String> {
        if cfg!(windows) {
            vec!["cmd".into(), "/C".into(), line]
        } else {
            vec!["sh".into(), "-c".into(), line]
        }
    }

    fn setup_for(&self, e: &Entry, kind: &str) -> Result<WorkerSetup, String> {
        Self::prepare(e)?;
        let npm = match e.name {
            "claude-code" => "@anthropic-ai/claude-code @zed-industries/claude-agent-acp@0.23",
            "codex" => "@openai/codex @zed-industries/codex-acp@0.16",
            "grok" => "@xai-official/grok@1.0.50",
            _ => return Err(format!("{} has no reviewed setup", e.title)),
        };
        let argv = match kind {
            "install" | "update" if e.name == "grok" => vec![
                "npm".into(),
                "install".into(),
                "--prefix".into(),
                e.isolated_home
                    .as_ref()
                    .expect("Grok has an isolated home")
                    .display()
                    .to_string(),
                npm.into(),
            ],
            "install" | "update" => Self::shell(format!("npm install -g {npm}")),
            "install_connector" => {
                let package = match e.name {
                    "claude-code" => "@zed-industries/claude-agent-acp@0.23",
                    "codex" => "@zed-industries/codex-acp@0.16",
                    _ => return Err(format!("{} needs no connector", e.title)),
                };
                Self::shell(format!("npm install -g {package}"))
            }
            "sign_in" => {
                let program = if e.provider_program == e.program {
                    Self::find(e)
                } else {
                    Self::find_named(e.provider_program)
                }
                .ok_or_else(|| format!("{} is not installed here", e.title))?;
                std::iter::once(program.display().to_string())
                    .chain(match e.name {
                        "codex" => vec!["login".to_string()],
                        "grok" => vec!["login".to_string(), "--device-auth".to_string()],
                        _ => Vec::new(),
                    })
                    .collect()
            }
            _ => return Err(format!("no setup action `{kind}` for {}", e.title)),
        };
        let cwd = e
            .isolated_home
            .clone()
            .or_else(ark_os::dirs::home)
            .unwrap_or_else(std::env::temp_dir);
        let clear_env = e.isolated_home.is_some();
        let env = e
            .isolated_home
            .as_ref()
            .map(|home| {
                let home = home.display().to_string();
                let mut env = BASE_ENV
                    .iter()
                    .filter_map(|name| {
                        std::env::var(name)
                            .ok()
                            .map(|value| ((*name).into(), value))
                    })
                    .collect::<Vec<_>>();
                env.extend([
                    ("HOME".into(), home.clone()),
                    ("GROK_HOME".into(), home),
                    ("GROK_AGENT_DASHBOARD".into(), "0".into()),
                    ("GROK_WORKFLOWS".into(), "0".into()),
                ]);
                env
            })
            .unwrap_or_default();
        Ok(WorkerSetup {
            argv,
            cwd,
            clear_env,
            env,
        })
    }
}

impl Workers for Catalog {
    fn worker(&self, name: &str) -> Option<Result<Arc<dyn Worker>, String>> {
        let e = self.entries.iter().find(|e| e.name == name)?;
        Some(match Catalog::find(e) {
            Some(program) => match Self::prepare(e) {
                Ok(()) => Ok(
                    Arc::new(Acp::new(e.clone(), program).hiding(self.hidden.clone()))
                        as Arc<dyn Worker>,
                ),
                Err(error) => Err(error),
            },
            None => Err(format!("{} is not set up here: {}", e.title, e.setup)),
        })
    }

    fn offers(&self) -> Vec<WorkerOffer> {
        self.entries
            .iter()
            .map(|entry| {
                if entry.name == "grok" {
                    Self::offer(entry)
                } else {
                    // Existing explicit specs keep working. They have no setup action and the
                    // chooser does not reveal them to people who have not already selected one.
                    Self::compatibility_offer(entry)
                }
            })
            .collect()
    }

    fn setup(&self, spec: &str, action: &str) -> Option<Result<WorkerSetup, String>> {
        let (name, _) = ark_runtime::worker::parse_spec(spec)?;
        let entry = self.entries.iter().find(|entry| entry.name == name)?;
        if entry.name != "grok" {
            return None;
        }
        let current = Self::offer(entry);
        let Some(offered) = current.action else {
            return Some(Err(format!("{} is already ready", entry.title)));
        };
        if offered.id != action {
            return Some(Err(format!(
                "{} setup changed; check it again before continuing",
                entry.title
            )));
        }
        Some(self.setup_for(entry, &offered.kind))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn installed_compatibility_workers_stay_hidden_but_usable() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("claude-code-acp");
        std::fs::write(&program, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::env::set_var("ARK_CLAUDE_CODE_ACP", &program);
        let catalog = Catalog::at(dir.path().join("workers"));

        let offer = catalog
            .offers()
            .into_iter()
            .find(|offer| offer.spec == "agent:claude-code")
            .unwrap();
        let worker = catalog.worker("claude-code").unwrap();
        std::env::remove_var("ARK_CLAUDE_CODE_ACP");

        assert!(!offer.discoverable);
        assert_eq!(offer.state, WorkerState::Ready);
        assert!(offer.action.is_none());
        assert!(worker.is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn probes_do_not_receive_ark_secrets() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let probe = dir.path().join("probe");
        std::fs::write(
            &probe,
            "#!/bin/sh\nprintf '%s' \"${ARK_RECOGNIZABLE_PROBE_SECRET-absent}\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::env::set_var("ARK_RECOGNIZABLE_PROBE_SECRET", "must-not-leak");
        let entry = entries_at(Some(dir.path()))
            .into_iter()
            .find(|entry| entry.name == "grok")
            .unwrap();

        let output = Catalog::output(&probe, &[], &entry).unwrap();
        std::env::remove_var("ARK_RECOGNIZABLE_PROBE_SECRET");

        assert_eq!(String::from_utf8_lossy(&output.stdout), "absent");
    }

    #[test]
    fn grok_setup_owns_its_home_and_fixed_policy() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = Catalog::at(dir.path());
        let grok = catalog
            .entries
            .iter()
            .find(|entry| entry.name == "grok")
            .unwrap();
        let home = dir.path().join("grok");
        let setup = catalog.setup_for(grok, "install").unwrap();

        assert_eq!(setup.cwd, home);
        assert!(setup.clear_env);
        assert!(setup
            .argv
            .iter()
            .any(|arg| arg == "@xai-official/grok@1.0.50"));
        assert!(setup.argv.iter().any(|arg| arg == "--prefix"));
        assert!(setup
            .env
            .contains(&("HOME".into(), home.display().to_string())));
        assert!(setup
            .env
            .iter()
            .all(|(name, _)| BASE_ENV.contains(&name.as_str())
                || matches!(
                    name.as_str(),
                    "HOME" | "GROK_HOME" | "GROK_AGENT_DASHBOARD" | "GROK_WORKFLOWS"
                )));
        let config = std::fs::read_to_string(home.join("config.toml")).unwrap();
        assert!(config.contains("permission_mode = \"ask\""));
        assert!(config.contains("ask = [\"Bash(*)\", \"Edit(*)\"]"));
        assert!(config.contains("enabled = false"));
    }

    #[test]
    fn catalogs_name_worker_models_without_runtime_words() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = Catalog::at(dir.path());
        let grok = catalog
            .entries
            .iter()
            .find(|entry| entry.name == "grok")
            .unwrap();
        let home = dir.path().join("grok");

        assert_eq!(grok.title, "Grok");
        assert!(grok
            .models
            .iter()
            .any(|(spec, title)| { *spec == "grok-4.7" && *title == "Grok 4.7" }));
        assert_eq!(grok.isolated_home.as_deref(), Some(home.as_path()));
    }
}
