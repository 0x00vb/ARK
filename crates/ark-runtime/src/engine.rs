//! The engine: ingest artifacts and run actions, recording every call as an invocation and
//! persisting everything it produces. This is where the approval gate is enforced: `Read`
//! and `Observe` run immediately; `Mutate` runs only with explicit approval. The agent loops
//! (`Agent`, `Harness`) are built on top of it.
//!
//! The engine registers **no** domain adapters: the consumer supplies a [`Registry`].
//! Content classification is pluggable via [`Sniffer`]; ARK ships only the text/json/binary
//! classifier.

use std::sync::Arc;

use ark_core::{
    detect_kind as core_detect_kind, Artifact, ArtifactId, ArtifactKind, Invocation,
    InvocationStatus, Project, ProjectId, Provenance, RiskTier, Sniffer,
};
use ark_store::Store;
use ark_tools::{Registry, RunCtx, ToolSource};

use crate::blobs::StoreBlobs;
use crate::cancel::{CancelToken, RunGuard, Stopping};
use crate::error::AgentError;
use crate::plugins::{PluginSpec, PluginStatus, Plugins};

/// The error of a call a stopped process left running. A state-changing one may or may not
/// have happened.
pub const INTERRUPTED_CALL: &str = "interrupted: the process stopped before this call finished";

pub struct Engine {
    /// Shared with every engine made by [`scoped`](Engine::scoped).
    pub store: Arc<Store>,
    /// The app's own adapters. MCP plugins are apart, in [`plugins`](Engine::plugins), because
    /// they change while the engine runs; [`tools`](Engine::tools) puts the two together.
    pub registry: Registry,
    sniffers: Vec<Arc<dyn Sniffer>>,
    /// The MCP plugins of `<root>/plugins.json`, for an engine opened on a root. A scoped engine
    /// has none of its own: its registry is exactly what it was given.
    plugins: Option<Arc<Plugins>>,
    /// Other sources of tools that change while the engine runs (connected accounts).
    sources: Vec<Arc<dyn ToolSource>>,
    /// Folders of skills that come with the app's tools ([`crate::skills::dirs`]).
    skills: Vec<std::path::PathBuf>,
    /// Shared with every engine made by [`scoped`](Engine::scoped): the process is stopping.
    stopping: Arc<Stopping>,
    /// Pictures a call showed, by invocation, until the harness takes them for the model.
    /// Never stored.
    pictures: Arc<std::sync::Mutex<std::collections::HashMap<String, Vec<ark_tools::Picture>>>>,
    /// The last pictures the harness took, by invocation, for a window to show in their step.
    shown: Arc<std::sync::Mutex<std::collections::VecDeque<(String, ark_tools::Picture)>>>,
    /// The work this engine's calls are made for ([`Invocation::work`]), set by
    /// [`for_work`](Engine::for_work). Kept by the engines scoped from it.
    work: Option<String>,
    /// Earlier runs of that work this one continues ([`continuing`](Engine::continuing)).
    continues: Vec<String>,
    /// What the agent said when it made the calls this engine runs ([`narrating`](Engine::narrating)).
    narration: Option<String>,
}

/// How many pictures stay for windows to show after the model got them.
const SHOWN: usize = 12;

impl Engine {
    /// Construct with a store and a consumer-supplied registry.
    pub fn new(store: Store, registry: Registry) -> Self {
        Engine {
            store: Arc::new(store),
            registry,
            sniffers: Vec::new(),
            plugins: None,
            sources: Vec::new(),
            skills: Vec::new(),
            stopping: Arc::default(),
            pictures: Arc::default(),
            shown: Arc::default(),
            work: None,
            continues: Vec::new(),
            narration: None,
        }
    }

    /// An engine over the same store with a different registry: the same projects, artifacts
    /// and records, another set of tools. An orchestrator gives each agent one, so an agent
    /// sees only the actions it is allowed and its own agent tools.
    pub fn scoped(&self, registry: Registry) -> Engine {
        Engine {
            store: self.store.clone(),
            registry,
            sniffers: self.sniffers.clone(),
            plugins: None,
            sources: Vec::new(),
            skills: self.skills.clone(),
            stopping: self.stopping.clone(),
            pictures: self.pictures.clone(),
            shown: self.shown.clone(),
            work: self.work.clone(),
            continues: self.continues.clone(),
            narration: self.narration.clone(),
        }
    }

    /// This engine, with every call it runs naming `work` ([`Invocation::work`]): a
    /// conversation's turn, so the calls of a turn that stopped can be found when it continues.
    pub fn for_work(&self, work: impl Into<String>) -> Engine {
        Engine {
            store: self.store.clone(),
            registry: self.registry.clone(),
            sniffers: self.sniffers.clone(),
            plugins: self.plugins.clone(),
            sources: self.sources.clone(),
            skills: self.skills.clone(),
            stopping: self.stopping.clone(),
            pictures: self.pictures.clone(),
            shown: self.shown.clone(),
            work: Some(work.into()),
            continues: self.continues.clone(),
            narration: self.narration.clone(),
        }
    }

    /// This engine, with every call it runs keeping what the agent said when it made it
    /// ([`Invocation::narration`], clipped to 600 characters). Kept in the call's record before
    /// it runs, so work that stops is continued with the agent's own plan (ADR 0050).
    pub fn narrating(&self, text: &str) -> Engine {
        let t = text.trim();
        let mut e = self.for_work(self.work.clone().unwrap_or_default());
        e.work = self.work.clone();
        e.narration = (!t.is_empty()).then(|| {
            if t.chars().count() <= 600 {
                t.to_string()
            } else {
                format!("{}…", t.chars().take(600).collect::<String>())
            }
        });
        e
    }

    /// This engine, telling the tools its work continues `works`, runs of it that stopped
    /// ([`RunCtx::continues`](ark_tools::RunCtx)).
    pub fn continuing(mut self, works: Vec<String>) -> Engine {
        self.continues = works;
        self
    }

    /// What the tools keep from `works` that a model continuing them should know
    /// ([`Adapter::continued`](ark_tools::Adapter::continued)), one line each.
    pub fn continued(&self, works: &[String]) -> Vec<String> {
        self.tools()
            .adapters()
            .iter()
            .flat_map(|a| a.continued(works))
            .collect()
    }

    /// The calls made for `work` in `project` ([`for_work`](Engine::for_work)), oldest first.
    pub fn work_calls(
        &self,
        project: &ProjectId,
        work: &str,
    ) -> Result<Vec<Invocation>, AgentError> {
        Ok(self.store.list_invocations_of_work(project, work)?)
    }

    /// The process is about to exit (a deploy, SIGTERM, the app quitting). Every run of this
    /// engine and the engines scoped from it stops at its next safe point: no new model or tool
    /// call starts, a model call or a batch of reads under way is dropped, and a batch with a
    /// state-changing call runs to its end, so a change is never cut in half. A run that starts
    /// now stops at once. Wait for them with [`runs_ended`](Engine::runs_ended).
    pub fn stop_soon(&self) {
        self.stopping.token.cancel();
    }

    pub fn is_stopping(&self) -> bool {
        self.stopping.token.is_cancelled()
    }

    /// The token a run watches for [`stop_soon`](Engine::stop_soon) (`Harness::stopping`).
    pub fn stopping(&self) -> CancelToken {
        self.stopping.token.clone()
    }

    /// Count a run in progress until the guard is dropped (the harness takes one per turn).
    pub fn run_started(&self) -> RunGuard {
        self.stopping.run()
    }

    /// Resolves once no run of this engine (or its scoped engines) is in progress.
    pub async fn runs_ended(&self) {
        self.stopping.idle().await
    }

    /// Add a folder of skills that come with a tool (Run code's Python comes with one for
    /// data work). They are found after the project's and the store's, so either can replace
    /// one by name.
    pub fn add_skills(&mut self, dir: impl Into<std::path::PathBuf>) {
        self.skills.push(dir.into());
    }

    /// The folders given to [`add_skills`](Engine::add_skills).
    pub fn skill_dirs(&self) -> &[std::path::PathBuf] {
        &self.skills
    }

    /// Everything this engine can run right now: its registry, then the plugins and the other
    /// sources as they are at this moment. A turn or task takes this once, when it starts, and
    /// keeps it. A source's adapter whose name is taken is left out.
    pub fn tools(&self) -> Registry {
        let mut registry = self.registry.clone();
        let mut taken: Vec<String> = registry
            .adapters()
            .iter()
            .map(|a| a.name().to_string())
            .collect();
        let plugins = self
            .plugins
            .iter()
            .map(|p| p.clone() as Arc<dyn ToolSource>);
        for source in plugins.chain(self.sources.iter().cloned()) {
            for a in source.adapters() {
                if !taken.iter().any(|n| n == a.name()) {
                    taken.push(a.name().to_string());
                    registry.register(a);
                }
            }
        }
        registry
    }

    /// Add a source of tools that change while the engine runs ([`ToolSource`]).
    pub fn add_source(&mut self, source: Arc<dyn ToolSource>) {
        self.sources.push(source);
    }

    /// Whether a turn's tools can differ from the registry (plugins or other sources).
    pub fn has_sources(&self) -> bool {
        self.plugins.is_some() || !self.sources.is_empty()
    }

    /// The pictures an invocation showed ([`ark_tools::ActionOutput::pictures`]), once.
    pub fn take_pictures(&self, invocation: &str) -> Vec<ark_tools::Picture> {
        let taken = self
            .pictures
            .lock()
            .unwrap()
            .remove(invocation)
            .unwrap_or_default();
        if let Some(p) = taken.first() {
            let mut shown = self.shown.lock().unwrap();
            shown.push_back((invocation.to_string(), p.clone()));
            while shown.len() > SHOWN {
                shown.pop_front();
            }
        }
        taken
    }

    /// The picture an invocation showed the model, while it is among the last few: what an
    /// agent saw, for a window to show in that step. Never stored.
    pub fn picture(&self, invocation: &str) -> Option<ark_tools::Picture> {
        let id = invocation.trim_start_matches("inv:");
        self.shown
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|(i, _)| i == id)
            .map(|(_, p)| p.clone())
    }

    /// An engine for one turn: this one, with the plugins connected now in its registry.
    pub fn for_turn(&self) -> Engine {
        self.scoped(self.tools())
    }

    /// The MCP plugins, when this engine was opened on a root (they live in its
    /// `plugins.json`).
    pub fn plugins(&self) -> Option<&Arc<Plugins>> {
        self.plugins.as_ref()
    }

    /// Connect a plugin and save it. Its name may not be one of the app's own adapters.
    pub fn add_plugin(&self, spec: PluginSpec) -> Result<PluginStatus, AgentError> {
        let plugins = self.plugins.as_ref().ok_or_else(|| {
            AgentError::Plugin("this app was not opened on a store root; it has no plugins".into())
        })?;
        let taken: Vec<String> = self
            .registry
            .adapters()
            .iter()
            .map(|a| a.name().to_string())
            .collect();
        plugins.add(spec, &taken)
    }

    /// Open a workspace at `root` with the given registry, and connect the MCP plugins
    /// `<root>/plugins.json` lists ([`Plugins`]).
    pub fn open_with(
        root: impl AsRef<std::path::Path>,
        registry: Registry,
    ) -> Result<Self, AgentError> {
        let root = root.as_ref().to_path_buf();
        let plugins = Plugins::load(&root);
        let mut engine = Engine::new(Store::open(root)?, registry);
        engine.plugins = Some(Arc::new(plugins));
        engine.recover_interrupted()?;
        Ok(engine)
    }

    /// Mark calls left running by a process that no longer exists as failed (their error is
    /// [`INTERRUPTED_CALL`]), so nothing reads
    /// as still in progress (or, worse, as finished) after a crash. Returns how many.
    pub fn recover_interrupted(&self) -> Result<usize, AgentError> {
        let mut n = 0;
        for (project, mut inv) in self.store.list_running_invocations()? {
            if inv
                .pid
                .is_some_and(|p| crate::process::writer_alive(p, inv.pid_start))
            {
                continue;
            }
            inv.mark_failed(INTERRUPTED_CALL);
            self.store.put_invocation(&project, &inv)?;
            n += 1;
        }
        // A chat's turn a stopped process left begun: opening its session marks it, so the
        // chat lists it, interrupted, before anyone opens it (ADR 0048).
        for id in self.store.sessions_with_begun_turns()? {
            if let Err(e) = crate::session::Session::open(self, &id) {
                tracing::warn!(session = %id, "an interrupted turn was not marked: {e}");
            }
        }
        Ok(n)
    }

    /// Open a workspace at `root` with an empty registry (plus MCP plugins). Register the
    /// consumer's adapters with [`register`](Engine::register) afterwards.
    pub fn open(root: impl AsRef<std::path::Path>) -> Result<Self, AgentError> {
        Engine::open_with(root, Registry::new())
    }

    /// Register a content sniffer (tried in order, before the built-in classifier).
    pub fn register_sniffer(&mut self, s: Arc<dyn Sniffer>) {
        self.sniffers.push(s);
    }

    /// Register an adapter on the engine's registry.
    pub fn register(&mut self, a: Arc<dyn ark_tools::Adapter>) {
        self.registry.register(a);
    }

    /// Classify bytes: consumer sniffers first, then the built-in text/json/binary detector.
    pub fn detect_kind(&self, name: &str, bytes: &[u8]) -> ArtifactKind {
        for s in &self.sniffers {
            if let Some(k) = s.sniff(name, bytes) {
                return k;
            }
        }
        core_detect_kind(bytes)
    }

    // ---- projects ---------------------------------------------------------

    pub fn create_project(&self, name: &str, target: &str) -> Result<Project, AgentError> {
        let p = Project::new(name, target);
        self.store.put_project(&p)?;
        Ok(p)
    }

    /// The on-disk workspace directory for a project's tools: the project's configured
    /// `workspace` if set, else a per-project scratch dir under the store root. Created if
    /// absent. A child project (an orchestrated agent's) works in its top-level project's
    /// folder, so changing that folder moves its agents too.
    pub fn workspace_dir(&self, project: &ProjectId) -> Result<std::path::PathBuf, AgentError> {
        let mut id = project.clone();
        let mut p = self.store.get_project(&id)?;
        // Bounded: a parent chain is one or two levels deep.
        for _ in 0..8 {
            match p.as_ref().and_then(|x| x.parent.clone()) {
                Some(parent) => {
                    p = self.store.get_project(&parent)?;
                    id = parent;
                }
                None => break,
            }
        }
        let configured = p.and_then(|p| p.workspace).map(std::path::PathBuf::from);
        let dir =
            configured.unwrap_or_else(|| self.store.root().join("workspaces").join(id.as_str()));
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    /// Set the folder a top-level project's tools work in (and its agents'). The folder must
    /// exist; it is stored canonical.
    pub fn set_workspace(
        &self,
        project: &ProjectId,
        dir: &std::path::Path,
    ) -> Result<Project, AgentError> {
        if !dir.is_dir() {
            return Err(AgentError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("{} is not a folder", dir.display()),
            )));
        }
        let mut p = self
            .store
            .get_project(project)?
            .ok_or_else(|| AgentError::ProjectNotFound(project.to_string()))?;
        p.workspace = Some(dir.canonicalize()?.to_string_lossy().into_owned());
        self.store.put_project(&p)?;
        Ok(p)
    }

    // ---- ingest -----------------------------------------------------------

    /// Read a local file, store its bytes content-addressed, and register it as an artifact
    /// with `Ingested` provenance. Idempotent on identical bytes.
    pub fn ingest_path(
        &self,
        project: &ProjectId,
        path: &std::path::Path,
    ) -> Result<Artifact, AgentError> {
        let bytes = std::fs::read(path)?;
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "artifact".into());
        let kind = self.detect_kind(&name, &bytes);
        self.ingest_bytes(
            project,
            &name,
            kind,
            &bytes,
            Provenance::Ingested {
                path: path.display().to_string(),
            },
        )
    }

    /// Store raw bytes as an artifact. Provenance is supplied by the caller.
    pub fn ingest_bytes(
        &self,
        project: &ProjectId,
        name: &str,
        kind: ArtifactKind,
        bytes: &[u8],
        provenance: Provenance,
    ) -> Result<Artifact, AgentError> {
        let art = self.keep_bytes(project, name, kind.clone(), bytes, provenance)?;
        // Index text-shaped artifacts so their contents become searchable evidence. Also index
        // a domain kind (`Other`) when its bytes are valid UTF-8 — a consumer's text-bearing
        // format (a plist, a JSON trace, an outline) should be searchable without ARK having to
        // know the format. Truly binary `Other` bytes fail the UTF-8 check and are skipped.
        let indexable = kind.is_textual() || matches!(kind, ArtifactKind::Other(_));
        if indexable {
            if let Ok(body) = std::str::from_utf8(bytes) {
                self.store
                    .index_text(project, &art.id, &art.name, &kind.as_tag(), body);
            }
        }
        Ok(art)
    }

    /// Store bytes as an artifact without indexing them: a file's version before a change is
    /// not evidence of what the file says now.
    fn keep_bytes(
        &self,
        project: &ProjectId,
        name: &str,
        kind: ArtifactKind,
        bytes: &[u8],
        provenance: Provenance,
    ) -> Result<Artifact, AgentError> {
        let id = self.store.put_blob(bytes)?;
        let art = Artifact::new(id, kind, name, bytes.len() as u64, provenance);
        self.store.put_artifact(project, &art)?;
        Ok(art)
    }

    /// Put a file of the project folder back as artifact `id` holds it: a version kept before a
    /// call changed or removed the file (ADR 0034). The file as it is now is kept first, the same
    /// way, so `undo` puts it back in turn. `undo` is `None` when there was no file.
    pub fn restore_file(
        &self,
        project: &ProjectId,
        id: &ArtifactId,
    ) -> Result<Restored, AgentError> {
        let art = self
            .store
            .get_artifact(project, id)?
            .ok_or_else(|| AgentError::ArtifactNotFound(id.to_string()))?;
        let rel = art.meta["path"]
            .as_str()
            .ok_or_else(|| invalid(format!("{} is not a file of the project", art.name)))?
            .to_string();
        let bytes = self.store.read_blob(&art.id)?;
        let file = project_file(&self.workspace_dir(project)?, &rel)?;
        let undo = match std::fs::read(&file) {
            Ok(now) => Some(
                self.keep_bytes(
                    project,
                    &art.name,
                    art.kind.clone(),
                    &now,
                    Provenance::Ingested { path: rel.clone() },
                )
                .and_then(|mut a| {
                    a.meta = serde_json::json!({ "path": rel, "before": true });
                    self.store.put_artifact(project, &a)?;
                    Ok(a.id)
                })?,
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // Whole or not at all: a program reading the file never sees half of it.
        let tmp = file.with_file_name(format!(
            ".ark-restore-{}-{}",
            art.id.short(),
            std::process::id()
        ));
        std::fs::write(&tmp, &bytes)?;
        if let Err(e) = std::fs::rename(&tmp, &file) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e.into());
        }
        Ok(Restored { path: rel, undo })
    }

    // ---- run actions ------------------------------------------------------

    /// Run an action and record it. The approval gate: `Read`/`Observe` run immediately;
    /// `Mutate` runs only if `allow_mutate` is true, otherwise the invocation is stored as
    /// `PendingApproval` and nothing executes.
    pub async fn run_action(
        &self,
        project: &ProjectId,
        action: &str,
        input: serde_json::Value,
        allow_mutate: bool,
    ) -> Result<Invocation, AgentError> {
        // A turn's engine has its plugins in its registry already; the root engine adds them.
        let registry = self.tools();
        let desc = registry
            .descriptor(action)
            .ok_or_else(|| AgentError::UnknownAction(action.to_string()))?;

        let input = self.resolve_artifact_arg(project, input);
        let mut inv = Invocation::new(action, desc.risk, input.clone());
        inv.work = self.work.clone();
        inv.narration = self.narration.clone();

        // record which artifact this call reads, if any, for the provenance graph
        if let Some(a) = input.get("artifact").and_then(|v| v.as_str()) {
            inv.consumed.push(ArtifactId(a.to_string()));
        }

        if desc.risk == RiskTier::Mutate && !allow_mutate {
            inv.status = InvocationStatus::PendingApproval;
            self.store.put_invocation(project, &inv)?;
            return Ok(inv);
        }
        // Recorded before it runs: a crash mid-call leaves a trace that recovery can mark.
        inv.status = InvocationStatus::Running;
        inv.pid = Some(std::process::id());
        inv.pid_start = crate::process::own_start();
        self.store.put_invocation(project, &inv)?;

        let blobs = StoreBlobs(&self.store);
        let ctx = RunCtx::new(&blobs)
            .with_workspace(self.workspace_dir(project)?)
            .with_work(self.work.clone(), self.continues.clone());

        match registry.run(action, input, &ctx).await {
            Ok(out) => {
                inv.command = out.command;
                if !out.pictures.is_empty() {
                    self.pictures
                        .lock()
                        .unwrap()
                        .insert(inv.id.as_str().to_string(), out.pictures);
                }
                self.keep_produced(project, &mut inv, out.produced)?;
                inv.mark_ok(out.output);
            }
            Err(e) => inv.mark_failed(e.to_string()),
        }

        self.store.put_invocation(project, &inv)?;
        Ok(inv)
    }
}

impl Engine {
    /// Store what a call produced on `inv`: written files as artifacts that name their file, a
    /// file's version before as a kept artifact (ADR 0034). For work recorded outside
    /// [`Engine::run_action`] (an agent's work elsewhere, ADR 0043).
    pub fn keep_produced(
        &self,
        project: &ProjectId,
        inv: &mut Invocation,
        produced: Vec<ark_tools::ProducedArtifact>,
    ) -> Result<(), AgentError> {
        for prod in produced {
            let provenance = prod.provenance.unwrap_or_else(|| Provenance::Produced {
                invocation: inv.id.clone(),
            });
            // A Blob is re-sniffed so produced bytes land as their real kind.
            let kind = if prod.kind == ArtifactKind::Blob {
                self.detect_kind(&prod.name, &prod.bytes)
            } else {
                prod.kind
            };
            // A file's version before the call is kept, not indexed: it is not what
            // the file says now.
            let mut art = if prod.before {
                self.keep_bytes(project, &prod.name, kind, &prod.bytes, provenance)?
            } else {
                self.ingest_bytes(project, &prod.name, kind, &prod.bytes, provenance)?
            };
            // A snapshot of a project file names the file, so a front-end opens the
            // file itself, as it is now. The file's version before the call names it
            // too, so a front-end can put it back (ADR 0034).
            if let Some(path) = prod.path {
                art.meta = if prod.before {
                    serde_json::json!({ "path": path, "before": true })
                } else {
                    serde_json::json!({ "path": path })
                };
                self.store.put_artifact(project, &art)?;
            }
            inv.produced.push(art.id);
        }
        Ok(())
    }

    /// The model reads artifacts as short ids (`art:dc3b0c93c72c`) and passes them back that
    /// way; adapters look blobs up by full id. Rewrite an `artifact` argument given as a full
    /// id, a short prefix (with or without `art:`) or an exact name to the full id. An
    /// argument that matches nothing is left as it was, so the adapter reports it.
    /// The input as the adapter will see it: an `artifact` argument resolved to its full id.
    /// Two calls with equal canonical inputs are the same call.
    pub fn canonical_input(
        &self,
        project: &ProjectId,
        input: serde_json::Value,
    ) -> serde_json::Value {
        self.resolve_artifact_arg(project, input)
    }

    fn resolve_artifact_arg(
        &self,
        project: &ProjectId,
        mut input: serde_json::Value,
    ) -> serde_json::Value {
        let Some(given) = input.get("artifact").and_then(|v| v.as_str()) else {
            return input;
        };
        let needle = given.trim().trim_start_matches("art:");
        if needle.is_empty() {
            return input;
        }
        if let Ok(Some(a)) = self.store.resolve_artifact(project, needle) {
            input["artifact"] = serde_json::Value::String(a.id.as_str().to_string());
        }
        input
    }
}

/// What [`Engine::restore_file`] did: the file it put back, and the version it replaced.
#[derive(Clone, Debug, serde::Serialize)]
pub struct Restored {
    pub path: String,
    pub undo: Option<ArtifactId>,
}

/// `rel` in the project folder `root`, or an error: no `..`, no absolute path, no link that
/// leads out.
fn project_file(root: &std::path::Path, rel: &str) -> Result<std::path::PathBuf, AgentError> {
    use std::path::{Component, Path};
    let out = || invalid(format!("`{rel}` is outside the project folder"));
    let r = Path::new(rel);
    if rel.is_empty() || r.components().any(|c| !matches!(c, Component::Normal(_))) {
        return Err(out());
    }
    let root = root.canonicalize()?;
    let path = root.join(r);
    // The nearest part that exists decides where the path really is.
    let mut probe = path.as_path();
    while !probe.exists() {
        probe = probe.parent().ok_or_else(out)?;
    }
    if !probe.canonicalize()?.starts_with(&root) {
        return Err(out());
    }
    Ok(path)
}

fn invalid(msg: String) -> AgentError {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, msg).into()
}
