//! # ark-exec
//!
//! ARK's Run code capability. An agent runs a program in the project folder, contained, and
//! every file the program writes there comes back as an artifact. `code.run` knows nothing
//! about charts, tables or reports: it says which files changed, the engine decides their kind,
//! and the window shows each kind with its view (ADR 0021).
//!
//! ```text
//! code.run {code, runtime}
//!   -> Runtime::ready            an environment ARK keeps (Python through uv)
//!   -> copies of the project     each file as it is, to keep the version before (ADR 0034)
//!   -> Sandbox                   reads: system, environment, project; writes: project; no network
//!   -> files the run wrote       -> ProducedArtifact with its path -> kind -> view
//!   -> files it changed/removed  -> ProducedArtifact::before, from the copies
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
pub use sandbox::{entry, Bounds, Sandbox};

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
    /// Skills that come with this runtime: a folder name and its `SKILL.md`. The SDK writes
    /// them where the conversation finds them ([`Exec::write_skills`]).
    fn skills(&self) -> Vec<(&'static str, &'static str)> {
        Vec::new()
    }
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
    /// Folders copied into the run's home folder (`HOME`) before it starts, by the name they
    /// get there: caches the runtime built when it was set up, which a run may write to.
    pub home: Vec<(PathBuf, &'static str)>,
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

    /// Write the skills of the runtimes into `dir`, one folder each, and remove the folders of
    /// skills no runtime has any more. `dir` is ARK's own: nothing else should be in it.
    pub fn write_skills(&self, dir: &Path) -> std::io::Result<()> {
        let skills: Vec<(&str, &str)> = self.runtimes.iter().flat_map(|r| r.skills()).collect();
        std::fs::create_dir_all(dir)?;
        for entry in std::fs::read_dir(dir)?.filter_map(Result::ok) {
            let name = entry.file_name();
            if entry.path().is_dir() && !skills.iter().any(|(n, _)| name == *n) {
                std::fs::remove_dir_all(entry.path())?;
            }
        }
        for (name, text) in skills {
            let file = dir.join(name).join("SKILL.md");
            if std::fs::read_to_string(&file).ok().as_deref() != Some(text) {
                std::fs::create_dir_all(dir.join(name))?;
                std::fs::write(&file, text)?;
            }
        }
        Ok(())
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
        for (from, name) in &launch.home {
            copy_dir(from, &tmp.join(name)).map_err(io)?;
        }
        let file = dir.join(launch.file);
        std::fs::write(&file, code).map_err(io)?;
        let bounds = Bounds {
            project: project.to_path_buf(),
            scratch: dir.clone(),
            reads: launch.reads.clone(),
        };
        let mut args = launch.args.clone();
        args.push(file.display().to_string());
        let (sandbox, program) = (self.sandbox.clone(), launch.program.clone());
        let wrapped =
            tokio::task::spawn_blocking(move || sandbox.wrap(&bounds, &program, &args)).await;
        let (program, args) = match wrapped {
            Ok(Ok(w)) => w,
            Ok(Err(e)) => {
                let _ = std::fs::remove_dir_all(&dir);
                return Err(AdapterError::ToolUnavailable {
                    tool: self.sandbox.name().into(),
                    detail: e,
                });
            }
            Err(e) => {
                let _ = std::fs::remove_dir_all(&dir);
                return Err(AdapterError::Tool(e.to_string()));
            }
        };
        let mut env = sandbox::base_env(&tmp, launch.program.parent());
        env.extend(launch.env.iter().cloned());
        let absent = sandbox::absent_kept(project);
        let (root, copies) = (project.to_path_buf(), dir.join("before"));
        let before = match tokio::task::spawn_blocking(move || Before::take(&root, &copies)).await {
            Ok(b) => b,
            Err(e) => {
                let _ = std::fs::remove_dir_all(&dir);
                return Err(AdapterError::Tool(e.to_string()));
            }
        };

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
        let removed = sandbox::remove_created(&absent);
        // Reads the copies of what changed (up to `BEFORE_TOTAL`): off the async threads.
        let root = project.to_path_buf();
        let kept = tokio::task::spawn_blocking(move || before.changed(&root)).await;
        let _ = std::fs::remove_dir_all(&dir);
        let kept = kept.map_err(|e| AdapterError::Tool(e.to_string()))?;
        let mut ran = ran?;
        if !removed.is_empty() {
            ran.stderr.push_str(&format!(
                "\nRemoved {}: a run may not create these folders in the project.",
                removed.join(", ")
            ));
        }

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
        produced.extend(kept.versions);
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
            "removed": kept.removed,
            "no_copy": kept.no_copy,
        });
        let mut out = ActionOutput::json(output)
            .with_command(vec![runtime.name().to_string(), launch.file.to_string()]);
        out.produced = produced;
        Ok(out)
    }
}

/// A file the run wrote, as a stored copy that names the file. Its kind is left to the engine's
/// sniffers, which decide it from the name the same way for every tool (`ark_files::Kinds`).
fn snapshot(rel: &str, bytes: Vec<u8>) -> ProducedArtifact {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    ProducedArtifact::bytes(ArtifactKind::Blob, name, bytes).at(rel)
}

fn bad(detail: String) -> AdapterError {
    AdapterError::BadInput {
        action: RUN.into(),
        detail,
    }
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    if !from.is_dir() {
        return Ok(());
    }
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)?.flatten() {
        let (src, dst) = (e.path(), to.join(e.file_name()));
        if e.file_type()?.is_dir() {
            copy_dir(&src, &dst)?;
        } else {
            std::fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}

fn io(e: std::io::Error) -> AdapterError {
    AdapterError::Io(e)
}

pub use ark_runtime::folder::written_since;
use ark_runtime::folder::Before;

#[async_trait]
impl Adapter for Exec {
    fn name(&self) -> &str {
        "code"
    }

    fn summary(&self) -> Option<&str> {
        Some("run a program, contained: the project's files only, no network, none of the person's programs")
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
             (paths relative to it) and has no network. It is contained: the person's own \
             programs (cargo, npm, git and the rest) are not there, so do not start them from \
             it. A project's own commands (its tests, a build) run with `process.start` when \
             it is offered; when it is not, say they cannot be run here.",
            runtimes.join("; ")
        );
        if !self.shows.is_empty() {
            summary.push_str(&format!(
                " Files it writes are shown to the person: {}.",
                self.shows
            ));
        }
        vec![ActionDescriptor::new(RUN, summary, RiskTier::Mutate).auto_eligible().with_input_schema(json!({
            "type": "object", "required": ["code"],
            "properties": {
                "purpose": { "type": "string", "description": "what the run does, in one short sentence the person understands (no code words), e.g. \"join the two sales files by customer and total each month\"" },
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
        let list = |k: &str| -> Vec<String> {
            out[k]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(Json::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default()
        };
        let files = list("files");
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
        let removed = list("removed");
        if !removed.is_empty() {
            t.push_str(&format!("\nremoved {}", removed.join(", ")));
        }
        let no_copy = list("no_copy");
        if !no_copy.is_empty() {
            t.push_str(&format!(
                "\nno copy of the version before (too large): {}",
                no_copy.join(", ")
            ));
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
