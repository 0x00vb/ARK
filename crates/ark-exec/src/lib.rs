//! # ark-exec
//!
//! ARK's Run code capability. An agent runs a program in the project folder, contained, and
//! every file the program writes there comes back as an artifact. `code.run` knows nothing
//! about charts, tables or reports: it says which files changed, the engine decides their kind,
//! and the window shows each kind with its view (ADR 0019).
//!
//! ```text
//! code.run {code, runtime}
//!   -> Runtime::ready            an environment ARK keeps (Python through uv)
//!   -> Sandbox                   reads: system, environment, project; writes: project; no network
//!   -> files the run wrote       -> ProducedArtifact with its path -> kind -> view
//! ```
//!
//! A runtime is one [`Runtime`]; [`Python`] is the first. Where ARK has no [`Sandbox`] the
//! capability is not offered: a run that could reach anything is not one a person can allow for
//! a whole project.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use ark_core::{ActionDescriptor, ArtifactKind, RiskTier};
use ark_tools::subprocess::{self, Spec};
use ark_tools::{ActionOutput, Adapter, AdapterError, AdapterHealth, ProducedArtifact, RunCtx};
use async_trait::async_trait;
use serde_json::{json, Value as Json};

pub mod python;
mod sandbox;

pub use python::Python;
pub use sandbox::{Bounds, Sandbox};

/// The action.
pub const RUN: &str = "code.run";

/// How long a run may take unless the agent asks for less or more, and the most it may ask.
const TIMEOUT: u64 = 120;
const MAX_TIMEOUT: u64 = 600;
/// What is kept of each output stream: the end, where results and errors are.
const KEEP: usize = 16_000;
/// What the agent sees of them in its card.
const SHOW_OUT: usize = 4_000;
const SHOW_ERR: usize = 2_000;
/// Files a run reports, files it returns as artifacts, and the largest one returned.
const MAX_LISTED: usize = 50;
const MAX_FILES: usize = 20;
const MAX_FILE_BYTES: u64 = 25 * 1024 * 1024;
/// Entries looked at when finding what a run wrote; a larger project is only partly looked at.
const MAX_WALK: usize = 20_000;
/// Folders never looked into for written files: tool and package caches.
const SKIP: &[&str] = &["node_modules", "target", "__pycache__", "venv", ".venv"];

/// One language or environment programs run in.
#[async_trait]
pub trait Runtime: Send + Sync {
    /// The name the agent passes (`python`).
    fn name(&self) -> &'static str;
    /// What it is, for the agent (`Python 3.12 with pandas, numpy, matplotlib, openpyxl`).
    fn says(&self) -> String;
    /// Whether it can run here, or why not, without installing anything.
    fn available(&self) -> Result<(), String>;
    /// Make the environment ready (install it the first time) and say how to start a program.
    async fn ready(&self) -> Result<Launch, String>;
}

/// How to start a program in a ready runtime.
pub struct Launch {
    pub program: PathBuf,
    /// Arguments before the program file.
    pub args: Vec<String>,
    /// The program file's name (`main.py`).
    pub file: &'static str,
    /// Environment variables besides the few every run gets.
    pub env: Vec<(String, String)>,
    /// Folders the run reads besides the system's and the project's (the environment).
    pub reads: Vec<PathBuf>,
}

/// The `code` tools: [`RUN`] in each of its runtimes.
pub struct Exec {
    runtimes: Vec<Arc<dyn Runtime>>,
    sandbox: Sandbox,
    /// Where each run gets its own folder (its program file and temporary files).
    scratch: PathBuf,
    /// What the window shows when a run writes it, for the agent ("charts as png, ...").
    shows: String,
}

impl Exec {
    /// The tools, with runs in `scratch`, or `None` where ARK cannot contain a run.
    pub fn new(scratch: impl Into<PathBuf>, runtimes: Vec<Arc<dyn Runtime>>) -> Option<Exec> {
        Some(Exec::with_sandbox(Sandbox::native()?, scratch, runtimes))
    }

    pub fn with_sandbox(
        sandbox: Sandbox,
        scratch: impl Into<PathBuf>,
        runtimes: Vec<Arc<dyn Runtime>>,
    ) -> Exec {
        Exec {
            runtimes,
            sandbox,
            scratch: scratch.into(),
            shows: String::new(),
        }
    }

    /// Say what the window shows when a run writes it (one line, from the formats ARK reads).
    pub fn shows(mut self, what: impl Into<String>) -> Self {
        self.shows = what.into();
        self
    }

    fn runtime(&self, name: Option<&str>) -> Result<&Arc<dyn Runtime>, AdapterError> {
        match name {
            None => self.runtimes.first(),
            Some(n) => self.runtimes.iter().find(|r| r.name() == n),
        }
        .ok_or_else(|| {
            let names: Vec<&str> = self.runtimes.iter().map(|r| r.name()).collect();
            bad(format!("runtime is one of: {}", names.join(", ")))
        })
    }

    async fn run(&self, input: &Json, project: &Path) -> Result<ActionOutput, AdapterError> {
        let code = input
            .get("code")
            .and_then(Json::as_str)
            .filter(|c| !c.trim().is_empty())
            .ok_or_else(|| bad("`code` is the program to run".into()))?;
        let runtime = self.runtime(input.get("runtime").and_then(Json::as_str))?;
        let secs = input
            .get("timeout")
            .and_then(Json::as_u64)
            .unwrap_or(TIMEOUT)
            .clamp(1, MAX_TIMEOUT);
        let launch = runtime
            .ready()
            .await
            .map_err(|e| AdapterError::ToolUnavailable {
                tool: runtime.name().into(),
                detail: e,
            })?;

        let dir = self.scratch.join(uuid::Uuid::new_v4().simple().to_string());
        let tmp = dir.join("tmp");
        std::fs::create_dir_all(&tmp).map_err(io)?;
        let file = dir.join(launch.file);
        std::fs::write(&file, code).map_err(io)?;
        let bounds = Bounds {
            project: project.to_path_buf(),
            scratch: dir.clone(),
            reads: launch.reads.clone(),
        };
        let mut args = launch.args.clone();
        args.push(file.display().to_string());
        let (program, args) = self.sandbox.wrap(&bounds, &launch.program, &args);
        let bin = launch
            .program
            .parent()
            .map(|p| format!("{}:", p.display()))
            .unwrap_or_default();
        let mut env = vec![
            ("HOME".to_string(), tmp.display().to_string()),
            ("TMPDIR".to_string(), format!("{}/", tmp.display())),
            ("PATH".to_string(), format!("{bin}/usr/bin:/bin")),
            ("LANG".to_string(), "en_US.UTF-8".to_string()),
        ];
        env.extend(launch.env.iter().cloned());

        let started = SystemTime::now();
        let ran = subprocess::run_spec(&Spec {
            program,
            args,
            cwd: Some(project.to_path_buf()),
            env: Some(env),
            timeout: Some(Duration::from_secs(secs)),
            keep: Some(KEEP),
        })
        .await;
        let _ = std::fs::remove_dir_all(&dir);
        let ran = ran?;

        let written = written_since(project, started);
        let mut produced = Vec::new();
        let mut not_kept = Vec::new();
        for (rel, size) in written.iter().take(MAX_LISTED) {
            if produced.len() < MAX_FILES && *size <= MAX_FILE_BYTES {
                if let Ok(bytes) = std::fs::read(project.join(rel)) {
                    produced.push(snapshot(rel, bytes));
                    continue;
                }
            }
            not_kept.push(rel.clone());
        }
        let files: Vec<&String> = written.iter().take(MAX_LISTED).map(|(p, _)| p).collect();
        let timed_out = ran.code == 124
            && ran
                .stderr
                .ends_with(&subprocess::timeout_note(Duration::from_secs(secs)));
        let output = json!({
            "runtime": runtime.name(),
            "exit_code": ran.code,
            "timed_out": timed_out,
            "stdout": ran.stdout,
            "stderr": ran.stderr,
            "files": files,
            "more_files": written.len().saturating_sub(MAX_LISTED),
            "not_kept": not_kept,
        });
        let mut out = ActionOutput::json(output)
            .with_command(vec![runtime.name().to_string(), launch.file.to_string()]);
        out.produced = produced;
        Ok(out)
    }
}

/// A file the run wrote, as an artifact. Its kind is left to the engine's sniffers, which
/// decide it from the path and the bytes the same way for every tool.
fn snapshot(rel: &str, bytes: Vec<u8>) -> ProducedArtifact {
    ProducedArtifact::bytes(ArtifactKind::Blob, rel, bytes)
}

fn bad(detail: String) -> AdapterError {
    AdapterError::BadInput {
        action: RUN.into(),
        detail,
    }
}

fn io(e: std::io::Error) -> AdapterError {
    AdapterError::Io(e)
}

/// Files under `root` modified at or after `since`, relative with `/`, newest first. Hidden
/// folders and package caches are not looked into.
pub fn written_since(root: &Path, since: SystemTime) -> Vec<(String, u64)> {
    let mut found: Vec<(String, u64, SystemTime)> = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    let mut seen = 0usize;
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            seen += 1;
            if seen > MAX_WALK {
                break;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                if !name.starts_with('.') && !SKIP.contains(&name.as_ref()) {
                    stack.push(entry.path());
                }
            } else if kind.is_file() {
                let Ok(meta) = entry.metadata() else {
                    continue;
                };
                let Ok(modified) = meta.modified() else {
                    continue;
                };
                if modified >= since {
                    let rel = entry
                        .path()
                        .strip_prefix(root)
                        .map(|p| {
                            p.components()
                                .map(|c| c.as_os_str().to_string_lossy())
                                .collect::<Vec<_>>()
                                .join("/")
                        })
                        .unwrap_or_default();
                    found.push((rel, meta.len(), modified));
                }
            }
        }
    }
    found.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)));
    found.into_iter().map(|(p, n, _)| (p, n)).collect()
}

#[async_trait]
impl Adapter for Exec {
    fn name(&self) -> &str {
        "code"
    }

    fn actions(&self) -> Vec<ActionDescriptor> {
        let runtimes: Vec<String> = self
            .runtimes
            .iter()
            .map(|r| format!("{} ({})", r.name(), r.says()))
            .collect();
        let names: Vec<&str> = self.runtimes.iter().map(|r| r.name()).collect();
        let mut summary = format!(
            "Run a program in the project folder and get its output and the files it wrote. \
             Runtimes: {}. The program reads and writes files only in the project folder \
             (paths relative to it) and has no network.",
            runtimes.join("; ")
        );
        if !self.shows.is_empty() {
            summary.push_str(&format!(
                " Files it writes are shown to the person: {}.",
                self.shows
            ));
        }
        vec![ActionDescriptor::new(RUN, summary, RiskTier::Mutate).with_input_schema(json!({
            "type": "object", "required": ["code"],
            "properties": {
                "code": { "type": "string", "description": "the whole program" },
                "runtime": { "type": "string", "enum": names, "description": format!("default {}", names.first().unwrap_or(&"")) },
                "timeout": { "type": "integer", "description": format!("seconds, default {TIMEOUT}, at most {MAX_TIMEOUT}") }
            }
        }))]
    }

    async fn health(&self) -> AdapterHealth {
        let why: Vec<String> = self
            .runtimes
            .iter()
            .filter_map(|r| r.available().err().map(|e| format!("{}: {e}", r.name())))
            .collect();
        if why.len() == self.runtimes.len() {
            AdapterHealth {
                available: false,
                detail: why.join("; "),
            }
        } else {
            AdapterHealth::ok()
        }
    }

    async fn run(
        &self,
        action: &str,
        input: Json,
        ctx: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        if action != RUN {
            return Err(AdapterError::UnknownAction(action.into()));
        }
        let project = ctx
            .workspace
            .clone()
            .ok_or_else(|| bad("there is no project folder to run in".into()))?;
        self.run(&input, &project).await
    }
}

/// What the agent reads of a run: the exit, the files written, and the end of each stream.
pub fn distill() -> ark_runtime::distill::Distiller {
    Arc::new(|action: &str, out: &Json| {
        if action != RUN {
            return None;
        }
        let s = |k: &str| {
            out.get(k)
                .and_then(Json::as_str)
                .unwrap_or("")
                .trim_end()
                .to_string()
        };
        let files: Vec<String> = out["files"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(Json::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let code = out["exit_code"].as_i64().unwrap_or(-1);
        let mut t = if out["timed_out"] == true {
            "stopped: it ran past its timeout".to_string()
        } else {
            format!("exit {code}")
        };
        if !files.is_empty() {
            t.push_str(&format!("\nwrote {}", files.join(", ")));
            if let Some(n) = out["more_files"].as_u64().filter(|n| *n > 0) {
                t.push_str(&format!(" and {n} more"));
            }
        }
        let stdout = s("stdout");
        if !stdout.is_empty() {
            t.push_str(&format!("\nstdout:\n{}", tail(&stdout, SHOW_OUT)));
        }
        let stderr = s("stderr");
        if !stderr.is_empty() && (code != 0 || stdout.is_empty()) {
            t.push_str(&format!("\nstderr:\n{}", tail(&stderr, SHOW_ERR)));
        }
        Some((t, files))
    })
}

fn tail(s: &str, n: usize) -> String {
    let count = s.chars().count();
    if count <= n {
        return s.to_string();
    }
    let rest: String = s.chars().skip(count - n).collect();
    format!("[{} earlier characters not shown]\n{rest}", count - n)
}
