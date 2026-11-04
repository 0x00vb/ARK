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
    detect_kind as core_detect_kind, Artifact, ArtifactId, ArtifactKind, ExternalEffectState,
    Invocation, InvocationId, InvocationStatus, Project, ProjectId, Provenance, RiskTier, Sniffer,
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

enum ActionAuthorization<'a> {
    Direct(bool),
    Reviewed {
        approved: bool,
        pending: Option<&'a Invocation>,
    },
}

#[derive(Clone)]
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
    project_scope: Option<ProjectId>,
    /// Folders of skills that come with the app's tools ([`crate::skills::dirs`]).
    skills: Vec<std::path::PathBuf>,
    skill_packages: Vec<Arc<crate::skills::packages::SkillPackage>>,
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
    guard: Option<Arc<dyn crate::guard::CallGuard>>,
}

/// How many pictures stay for windows to show after the model got them.
const SHOWN: usize = 12;

fn descriptor_fingerprint(descriptor: &ark_core::ActionDescriptor) -> Result<String, AgentError> {
    Ok(blake3::hash(&serde_json::to_vec(descriptor)?)
        .to_hex()
        .to_string())
}

impl Engine {
    pub fn guarded(&self, guard: Arc<dyn crate::guard::CallGuard>) -> Self {
        let mut engine = self.clone();
        engine.guard = Some(crate::guard::narrow(engine.guard.take(), guard));
        engine
    }

    pub fn call_guard(&self) -> Option<Arc<dyn crate::guard::CallGuard>> {
        self.guard.clone()
    }

    /// Apply trusted external-effect outcomes before any model answer is stored or shown. A
    /// tool can report an unconfirmed effect that must not be turned into a success claim by
    /// prose. A later confirmed marker for the same id supersedes it.
    pub fn finalize_answer(
        &self,
        project: &ProjectId,
        cited: &[InvocationId],
        answer: impl Into<String>,
    ) -> Result<String, AgentError> {
        let mut effects = std::collections::BTreeMap::new();
        let invocations = if let Some(work) = &self.work {
            self.store.list_invocations_of_work(project, work)?
        } else {
            cited
                .iter()
                .filter_map(|id| self.store.get_invocation(project, id.as_str()).transpose())
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        for inv in invocations {
            let Some(effect) = inv.effect else {
                continue;
            };
            effects.insert(effect.id.clone(), effect);
        }
        if let Some(effect) = effects
            .values()
            .rev()
            .find(|effect| effect.state == ExternalEffectState::Unconfirmed)
        {
            if let Some(message) = effect.user_message.as_ref().filter(|text| !text.is_empty()) {
                return Ok(message.clone());
            }
        }
        Ok(answer.into())
    }

    /// Construct with a store and a consumer-supplied registry.
    pub fn new(store: Store, registry: Registry) -> Self {
        Engine {
            store: Arc::new(store),
            registry,
            sniffers: Vec::new(),
            plugins: None,
            sources: Vec::new(),
            project_scope: None,
            skills: Vec::new(),
            skill_packages: Vec::new(),
            stopping: Arc::default(),
            pictures: Arc::default(),
            shown: Arc::default(),
            work: None,
            continues: Vec::new(),
            narration: None,
            guard: None,
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
            project_scope: self.project_scope.clone(),
            skills: self.skills.clone(),
            skill_packages: self.skill_packages.clone(),
            stopping: self.stopping.clone(),
            pictures: self.pictures.clone(),
            shown: self.shown.clone(),
            work: self.work.clone(),
            continues: self.continues.clone(),
            narration: self.narration.clone(),
            guard: self.guard.clone(),
        }
    }

    /// A registry view bound to a project. Existing narrower scope cannot be widened.
    pub fn scoped_for(&self, project: &ProjectId, registry: Registry) -> Engine {
        let mut engine = self.scoped(registry);
        if engine.project_scope.is_none() {
            engine.project_scope = Some(project.clone());
        }
        engine
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
            project_scope: self.project_scope.clone(),
            skills: self.skills.clone(),
            skill_packages: self.skill_packages.clone(),
            stopping: self.stopping.clone(),
            pictures: self.pictures.clone(),
            shown: self.shown.clone(),
            work: Some(work.into()),
            continues: self.continues.clone(),
            narration: self.narration.clone(),
            guard: self.guard.clone(),
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

    /// Register reviewed data-only skills for person-directed installation.
    pub fn add_skill_package(
        &mut self,
        package: crate::skills::packages::SkillPackage,
    ) -> Result<(), AgentError> {
        package.validate()?;
        if self.skill_packages.len() >= 128
            || self.skill_packages.iter().any(|p| p.id == package.id)
        {
            return Err(AgentError::Plugin(
                "Duplicate or excessive skill package registration".into(),
            ));
        }
        self.skill_packages.push(Arc::new(package));
        Ok(())
    }

    pub fn skill_packages(&self) -> &[Arc<crate::skills::packages::SkillPackage>] {
        &self.skill_packages
    }

    /// The folders given to [`add_skills`](Engine::add_skills).
    pub fn skill_dirs(&self) -> &[std::path::PathBuf] {
        &self.skills
    }

    /// Everything this engine can run right now: its registry, then the plugins and the other
    /// sources as they are at this moment. A turn or task takes this once, when it starts, and
    /// keeps it. A source's adapter whose name is taken is left out.
    pub fn tools(&self) -> Registry {
        self.tools_for_scope(None)
    }

    /// Snapshot dynamic sources for one project.
    pub fn tools_for(&self, project: &ProjectId) -> Registry {
        if self
            .project_scope
            .as_ref()
            .is_some_and(|scope| scope != project)
        {
            return Registry::new();
        }
        self.tools_for_scope(Some(project))
    }

    fn tools_for_scope(&self, project: Option<&ProjectId>) -> Registry {
        self.source_snapshot(project).0
    }

    /// Names a reserved source cannot use because another source or plugin owns them.
    pub fn source_conflicts_for(&self, project: &ProjectId) -> Vec<String> {
        self.source_snapshot(Some(project)).1
    }

    fn source_snapshot(&self, project: Option<&ProjectId>) -> (Registry, Vec<String>) {
        let sources: Vec<_> = self
            .sources
            .iter()
            .map(|source| {
                let adapters = match project {
                    Some(project) => source.adapters_for(project.as_str()),
                    None => source.adapters(),
                };
                (source.reserved_names(), adapters)
            })
            .collect();
        let mut conflicts = std::collections::BTreeSet::new();
        for (owner, (names, _)) in sources.iter().enumerate() {
            for name in names {
                let in_registry = self
                    .registry
                    .adapters()
                    .iter()
                    .any(|adapter| adapter.name() == name);
                let in_plugin = self.plugins.as_ref().is_some_and(|plugins| {
                    plugins.list().iter().any(|plugin| &plugin.name == name)
                });
                let in_source = sources
                    .iter()
                    .enumerate()
                    .any(|(index, (reserved, adapters))| {
                        index != owner
                            && (reserved.contains(name)
                                || adapters.iter().any(|adapter| adapter.name() == name))
                    });
                if in_registry || in_plugin || in_source {
                    conflicts.insert(name.clone());
                }
            }
        }
        let mut registry = self.registry.filtered(|action| {
            let namespace = action.split('.').next().unwrap_or(action);
            !conflicts.contains(namespace)
        });
        let mut taken: std::collections::BTreeSet<String> = registry
            .adapters()
            .iter()
            .map(|adapter| adapter.name().to_owned())
            .collect();
        if let Some(plugins) = &self.plugins {
            for adapter in plugins.adapters() {
                if !conflicts.contains(adapter.name()) && taken.insert(adapter.name().to_owned()) {
                    registry.register(adapter);
                }
            }
        }
        for (_, adapters) in sources {
            for adapter in adapters {
                if conflicts.contains(adapter.name()) {
                    continue;
                }
                if taken.insert(adapter.name().to_owned()) {
                    registry.register(adapter);
                }
            }
        }
        (registry, conflicts.into_iter().collect())
    }

    /// Add a source of tools that change while the engine runs ([`ToolSource`]).
    pub fn add_source(&mut self, source: Arc<dyn ToolSource>) {
        if source.reserved_names().is_empty() {
            self.sources.push(source);
        } else if let Err(error) = self.try_add_source(source) {
            tracing::error!(%error, "tool source could not be registered");
        }
    }

    /// Register a source only when its reserved names are not already in use.
    pub fn try_add_source(&mut self, source: Arc<dyn ToolSource>) -> Result<(), AgentError> {
        let names = source.reserved_names();
        let mut taken: std::collections::BTreeSet<String> = self
            .registry
            .adapters()
            .iter()
            .map(|a| a.name().to_owned())
            .collect();
        if let Some(plugins) = &self.plugins {
            taken.extend(plugins.list().into_iter().map(|p| p.name));
        }
        for existing in &self.sources {
            taken.extend(existing.reserved_names());
            taken.extend(existing.adapters().into_iter().map(|a| a.name().to_owned()));
        }
        for name in names {
            if name.is_empty() || !taken.insert(name.clone()) {
                return Err(AgentError::Plugin(format!(
                    "Tool namespace `{name}` is already in use"
                )));
            }
        }
        self.sources.push(source);
        Ok(())
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

    /// A turn snapshot whose dynamic sources are scoped to its project.
    pub fn for_project_turn(&self, project: &ProjectId) -> Engine {
        self.scoped_for(project, self.tools_for(project))
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
        let mut taken: Vec<String> = self
            .registry
            .adapters()
            .iter()
            .map(|a| a.name().to_string())
            .collect();
        taken.extend(
            self.sources
                .iter()
                .flat_map(|source| source.reserved_names()),
        );
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
        for observed in self.store.list_running_invocations_for_recovery()? {
            let inv = observed.invocation();
            if inv
                .pid
                .is_some_and(|p| crate::process::writer_alive(p, inv.pid_start))
            {
                continue;
            }
            if self
                .store
                .interrupt_invocation_if_unchanged(&observed, INTERRUPTED_CALL)?
            {
                n += 1;
            }
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
        if let Err(error) = self.try_register(a) {
            tracing::error!(%error, "tool adapter could not be registered");
        }
    }

    pub fn try_register(&mut self, a: Arc<dyn ark_tools::Adapter>) -> Result<(), AgentError> {
        if self
            .sources
            .iter()
            .any(|source| source.reserved_names().iter().any(|name| name == a.name()))
        {
            return Err(AgentError::Plugin(format!(
                "Tool namespace `{}` is reserved",
                a.name()
            )));
        }
        self.registry.register(a);
        Ok(())
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
        p.workspace = Some(
            ark_os::dirs::canonicalize(dir)?
                .to_string_lossy()
                .into_owned(),
        );
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

    /// Keep file `rel` of the project folder, as it is now, as a version before (ADR 0034): an
    /// artifact with meta `{path, before: true}` that [`Engine::restore_file`] puts back. For a
    /// change about to be made outside a call, such as a project copy brought back (ADR 0052).
    /// `None` when there is no file. A path out of the folder is refused.
    pub fn keep_before(
        &self,
        project: &ProjectId,
        rel: &str,
    ) -> Result<Option<ArtifactId>, AgentError> {
        let file = project_file(&self.workspace_dir(project)?, rel)?;
        let now = match std::fs::read(&file) {
            Ok(now) => now,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let name = rel.rsplit('/').next().unwrap_or(rel);
        let kind = self.detect_kind(name, &now);
        let mut a = self.keep_bytes(
            project,
            name,
            kind,
            &now,
            Provenance::Ingested {
                path: rel.to_string(),
            },
        )?;
        a.meta = serde_json::json!({ "path": rel, "before": true });
        self.store.put_artifact(project, &a)?;
        Ok(Some(a.id))
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
        let undo = self.keep_before(project, &rel)?;
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

    pub(crate) fn pending_approval(
        &self,
        project: &ProjectId,
        action: &str,
        input: serde_json::Value,
    ) -> Result<Invocation, AgentError> {
        let descriptor = self
            .tools()
            .descriptor(action)
            .ok_or_else(|| AgentError::UnknownAction(action.into()))?;
        if descriptor.risk != RiskTier::Mutate {
            return Err(AgentError::NotAllowed(
                "only a change can await approval".into(),
            ));
        }
        let input = self.action_input(project, input)?;
        if let Some(guard) = &self.guard {
            if let crate::guard::Verdict::Refuse(reason) =
                guard.check(action, &input, Some(&descriptor))
            {
                return Err(AgentError::NotAllowed(reason));
            }
        }
        let mut invocation = Invocation::new(action, descriptor.risk, input);
        invocation.status = InvocationStatus::PendingApproval;
        invocation.descriptor = Some(descriptor_fingerprint(&descriptor)?);
        invocation.work = self.work.clone();
        invocation.narration = self.narration.clone();
        self.store.create_invocation(project, &invocation)?;
        Ok(invocation)
    }

    fn action_input(
        &self,
        project: &ProjectId,
        mut input: serde_json::Value,
    ) -> Result<serde_json::Value, AgentError> {
        if self.guard.is_some() {
            if let Some(needle) = input.get("artifact").and_then(serde_json::Value::as_str) {
                let artifact = self
                    .store
                    .resolve_artifact(project, needle.trim_start_matches("art:"))?
                    .ok_or_else(|| AgentError::ArtifactNotFound(needle.into()))?;
                input["artifact"] = serde_json::Value::String(artifact.id.to_string());
            }
            Ok(input)
        } else {
            Ok(self.resolve_artifact_arg(project, input))
        }
    }

    pub(crate) fn deny_pending(
        &self,
        project: &ProjectId,
        pending: &Invocation,
    ) -> Result<(), AgentError> {
        let mut denied = pending.clone();
        denied.status = InvocationStatus::Denied;
        denied.finished_at = Some(time::OffsetDateTime::now_utc());
        if !self
            .store
            .transition_invocation(project, pending, &denied)?
        {
            return Err(AgentError::NotAllowed(
                "the approval's action changed before it was declined".into(),
            ));
        }
        Ok(())
    }

    /// Resume an exact stored call through its approval gate. Running calls require review.
    pub async fn run_pending_with_approval(
        &self,
        project: &ProjectId,
        id: &InvocationId,
        gate: &crate::approval::ApprovalGate,
    ) -> Result<Invocation, AgentError> {
        let pending = self
            .store
            .get_invocation(project, id.as_str())?
            .ok_or_else(|| AgentError::NotAllowed("the pending action is unavailable".into()))?;
        if pending.work != self.work {
            return Err(AgentError::NotAllowed(
                "the pending action belongs to another work item".into(),
            ));
        }
        if matches!(
            pending.status,
            InvocationStatus::Ok | InvocationStatus::Failed | InvocationStatus::Denied
        ) {
            return Ok(pending);
        }
        if pending.status != InvocationStatus::PendingApproval {
            return Err(AgentError::NotAllowed(
                "this action may already have run; review its result before continuing".into(),
            ));
        }
        let descriptor = self
            .tools()
            .descriptor(&pending.action)
            .ok_or_else(|| AgentError::UnknownAction(pending.action.clone()))?;
        if pending.descriptor.as_deref() != Some(descriptor_fingerprint(&descriptor)?.as_str()) {
            return Err(AgentError::NotAllowed(
                "the pending action changed since it was reviewed".into(),
            ));
        }
        if let Some(guard) = &self.guard {
            if let crate::guard::Verdict::Refuse(reason) =
                guard.check(&pending.action, &pending.input, Some(&descriptor))
            {
                return Err(AgentError::NotAllowed(reason));
            }
        }
        let request = crate::approval::ApprovalRequest {
            invocation: Some(pending.id.clone()),
            project: Some(project.clone()),
            action: pending.action.clone(),
            summary: descriptor.summary.clone(),
            risk: pending.risk,
            input: pending.input.clone(),
            class: descriptor
                .grant
                .as_ref()
                .and_then(|grant| grant.class(&pending.input))
                .unwrap_or_else(|| pending.action.clone()),
            preview: crate::approval::preview_call(&pending.action, &pending.input),
            agent: None,
            task: None,
            ask_each_time: true,
            cut_short: false,
        };
        if !gate.check(&request).await.allows() {
            self.deny_pending(project, &pending)?;
            return self
                .store
                .get_invocation(project, id.as_str())?
                .ok_or_else(|| {
                    AgentError::NotAllowed("the declined action is unavailable".into())
                });
        }
        self.run_action_authorized(
            project,
            &pending.action.clone(),
            pending.input.clone(),
            ActionAuthorization::Reviewed {
                approved: true,
                pending: Some(&pending),
            },
            None,
        )
        .await
    }

    pub async fn run_action(
        &self,
        project: &ProjectId,
        action: &str,
        input: serde_json::Value,
        allow_mutate: bool,
    ) -> Result<Invocation, AgentError> {
        self.run_action_authorized(
            project,
            action,
            input,
            ActionAuthorization::Direct(allow_mutate),
            None,
        )
        .await
    }

    pub(crate) async fn run_approved_action(
        &self,
        project: &ProjectId,
        action: &str,
        input: serde_json::Value,
        approved: bool,
        pending: Option<Invocation>,
        fence: Option<Arc<dyn ark_tools::CommitFence>>,
    ) -> Result<Invocation, AgentError> {
        self.run_action_authorized(
            project,
            action,
            input,
            ActionAuthorization::Reviewed {
                approved,
                pending: pending.as_ref(),
            },
            fence,
        )
        .await
    }

    pub async fn run_action_with_fence(
        &self,
        project: &ProjectId,
        action: &str,
        input: serde_json::Value,
        allow_mutate: bool,
        fence: Option<Arc<dyn ark_tools::CommitFence>>,
    ) -> Result<Invocation, AgentError> {
        self.run_action_authorized(
            project,
            action,
            input,
            ActionAuthorization::Direct(allow_mutate),
            fence,
        )
        .await
    }

    async fn run_action_authorized(
        &self,
        project: &ProjectId,
        action: &str,
        input: serde_json::Value,
        authorization: ActionAuthorization<'_>,
        fence: Option<Arc<dyn ark_tools::CommitFence>>,
    ) -> Result<Invocation, AgentError> {
        let (allow_mutate, guarded_approval, pending) = match authorization {
            ActionAuthorization::Direct(allow) => (allow, false, None),
            ActionAuthorization::Reviewed { approved, pending } => {
                (approved, true, pending.cloned())
            }
        };
        if self
            .project_scope
            .as_ref()
            .is_some_and(|scope| scope != project)
        {
            return Err(AgentError::ProjectScopeMismatch);
        }
        // A turn's engine has its plugins in its registry already; the root engine adds them.
        let registry = self.tools_for(project);
        let desc = registry
            .descriptor(action)
            .ok_or_else(|| AgentError::UnknownAction(action.to_string()))?;

        let mut inv = if let Some(pending) = &pending {
            if pending.action != action
                || pending.input != input
                || pending.work != self.work
                || pending.risk != desc.risk
                || pending.descriptor.as_deref() != Some(descriptor_fingerprint(&desc)?.as_str())
            {
                return Err(AgentError::NotAllowed(
                    "the reviewed action changed before execution".into(),
                ));
            }
            pending.clone()
        } else {
            let input = self.action_input(project, input)?;
            let mut invocation = Invocation::new(action, desc.risk, input);
            invocation.descriptor = Some(descriptor_fingerprint(&desc)?);
            invocation.work = self.work.clone();
            invocation.narration = self.narration.clone();
            invocation
        };
        let input = inv.input.clone();
        // record which artifact this call reads, if any, for the provenance graph
        if let Some(a) = input.get("artifact").and_then(|v| v.as_str()) {
            inv.consumed.push(ArtifactId(a.to_string()));
        }

        let allow_mutate = match &self.guard {
            Some(guard) => match guard.check(action, &input, Some(&desc)) {
                crate::guard::Verdict::Refuse(reason) => {
                    return Err(AgentError::NotAllowed(reason))
                }
                crate::guard::Verdict::Ask if desc.risk != RiskTier::Mutate => {
                    return Err(AgentError::NotAllowed(
                        "this operation has no guarded approval route".into(),
                    ))
                }
                crate::guard::Verdict::Ask => guarded_approval && allow_mutate,
                crate::guard::Verdict::Allow => {
                    !desc.person_only || (guarded_approval && allow_mutate)
                }
            },
            None => allow_mutate && !desc.person_only || guarded_approval && allow_mutate,
        };
        if desc.risk == RiskTier::Mutate && !allow_mutate {
            inv.status = InvocationStatus::PendingApproval;
            self.store.create_invocation(project, &inv)?;
            return Ok(inv);
        }
        // Recorded before it runs: a crash mid-call leaves a trace that recovery can mark.
        inv.status = InvocationStatus::Running;
        inv.pid = Some(std::process::id());
        inv.pid_start = crate::process::own_start();
        let mut reservation = self
            .guard
            .clone()
            .map(|guard| crate::guard::Reservation::new(guard, project.clone(), inv.clone()));
        if let Some(reservation) = &mut reservation {
            reservation.prepare().map_err(AgentError::NotAllowed)?;
        }
        if let Some(pending) = &pending {
            if !self.store.transition_invocation(project, pending, &inv)? {
                return Err(AgentError::NotAllowed(
                    "another execution already claimed this approval".into(),
                ));
            }
        } else {
            self.store.create_invocation(project, &inv)?;
        }
        if let Some(reservation) = &mut reservation {
            reservation.started().map_err(AgentError::NotAllowed)?;
        }

        let publication_intent = Arc::new(ark_tools::AtomicPublicationIntent::default());
        let blobs = StoreBlobs(&self.store);
        let ctx = RunCtx::new(&blobs)
            .with_workspace(self.workspace_dir(project)?)
            .with_work(self.work.clone(), self.continues.clone())
            .with_invocation(inv.id.as_str())
            .with_project(project.as_str())
            .with_commit_fence(fence)
            .with_atomic_publication(publication_intent.clone());

        let outcome = registry.run(action, input, &ctx).await;
        if publication_intent.requested() {
            let terminal = if let Some(committed) = self.committed_invocation(project, &inv)? {
                committed
            } else {
                let message = match outcome {
                    Ok(_) => "Atomic publication was not committed".to_string(),
                    Err(error) => error.to_string(),
                };
                if self
                    .store
                    .fail_running_invocation_if_unchanged(project, &inv, &message)?
                {
                    self.store
                        .get_invocation(project, inv.id.as_str())?
                        .ok_or(ark_store::StoreError::ImmutableConflict)?
                } else {
                    self.committed_invocation(project, &inv)?
                        .ok_or(ark_store::StoreError::ImmutableConflict)?
                }
            };
            if let Some(reservation) = &mut reservation {
                reservation
                    .finish(&terminal)
                    .map_err(AgentError::NotAllowed)?;
            }
            return Ok(terminal);
        }
        match outcome {
            Ok(out) => {
                inv.command = out.command;
                inv.effect = out.effect;
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
        if let Some(reservation) = &mut reservation {
            reservation.finish(&inv).map_err(AgentError::NotAllowed)?;
        }
        Ok(inv)
    }

    fn committed_invocation(
        &self,
        project: &ProjectId,
        running: &Invocation,
    ) -> Result<Option<Invocation>, AgentError> {
        let Some(publication) = self
            .store
            .load_output_publication(project, running.id.as_str())?
        else {
            return Ok(None);
        };
        let result = publication.invocation;
        if result.id != running.id
            || result.action != running.action
            || result.risk != running.risk
            || result.input != running.input
            || result.consumed != running.consumed
            || result.created_at != running.created_at
            || result.pid != running.pid
            || result.pid_start != running.pid_start
            || result.work != running.work
            || result.narration != running.narration
        {
            return Err(ark_store::StoreError::ImmutableConflict.into());
        }
        Ok(Some(result))
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

#[cfg(test)]
mod effect_tests {
    use super::*;
    use serde_json::json;

    fn effect(state: &str, message: &str, work: Option<&str>) -> Invocation {
        let mut invocation = Invocation::new("computer.send", RiskTier::Mutate, json!({}));
        invocation.work = work.map(str::to_string);
        invocation.effect = Some(ark_core::ExternalEffect {
            id: "message-1".into(),
            state: if state == "confirmed" {
                ExternalEffectState::Confirmed
            } else {
                ExternalEffectState::Unconfirmed
            },
            retry_allowed: false,
            user_message: (!message.is_empty()).then(|| message.to_string()),
        });
        invocation.mark_ok(json!({}));
        invocation
    }

    #[test]
    fn an_unconfirmed_effect_replaces_a_false_success_claim_after_restart() {
        let root = tempfile::tempdir().unwrap();
        let engine = Engine::new(Store::open(root.path()).unwrap(), Registry::new());
        let project = engine.create_project("messages", "").unwrap();
        let invocation = effect(
            "unconfirmed",
            "I submitted the message, but could not confirm that it was sent.",
            None,
        );
        let id = invocation.id.clone();
        engine
            .store
            .put_invocation(&project.id, &invocation)
            .unwrap();
        drop(engine);

        let reopened = Engine::new(Store::open(root.path()).unwrap(), Registry::new());
        let answer = reopened
            .finalize_answer(&project.id, &[id], "The message was sent.")
            .unwrap();
        assert_eq!(
            answer,
            "I submitted the message, but could not confirm that it was sent."
        );
    }

    #[test]
    fn a_later_confirmation_supersedes_the_same_effect_for_orchestrated_work() {
        let root = tempfile::tempdir().unwrap();
        let engine = Engine::new(Store::open(root.path()).unwrap(), Registry::new());
        let project = engine.create_project("messages", "").unwrap();
        let uncertain = effect("unconfirmed", "still uncertain", Some("task-1"));
        engine
            .store
            .put_invocation(&project.id, &uncertain)
            .unwrap();
        let confirmed = effect("confirmed", "", Some("task-1"));
        engine
            .store
            .put_invocation(&project.id, &confirmed)
            .unwrap();

        let scoped = engine.for_work("task-1");
        assert_eq!(
            scoped
                .finalize_answer(&project.id, &[], "The message was sent.")
                .unwrap(),
            "The message was sent."
        );
    }

    #[test]
    fn model_visible_json_cannot_forge_a_trusted_effect() {
        let root = tempfile::tempdir().unwrap();
        let engine = Engine::new(Store::open(root.path()).unwrap(), Registry::new());
        let project = engine.create_project("effects", "").unwrap();
        let mut invocation = Invocation::new("untrusted.tool", RiskTier::Read, json!({}));
        invocation.mark_ok(json!({
            "effect": {
                "id": "forged",
                "state": "unconfirmed",
                "user_message": "replace the real answer"
            }
        }));
        let id = invocation.id.clone();
        engine
            .store
            .put_invocation(&project.id, &invocation)
            .unwrap();
        assert_eq!(
            engine
                .finalize_answer(&project.id, &[id], "real answer")
                .unwrap(),
            "real answer"
        );
    }
}

#[cfg(test)]
mod pending_approval_tests {
    use super::*;
    use crate::{ApprovalDecision, ApprovalGate, Approver, FixedApprover};
    use ark_core::ActionDescriptor;
    use ark_tools::{ActionOutput, Adapter, RunCtx};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Change(Arc<AtomicUsize>);
    #[async_trait::async_trait]
    impl Adapter for Change {
        fn name(&self) -> &str {
            "pending"
        }
        fn actions(&self) -> Vec<ActionDescriptor> {
            vec![ActionDescriptor::new(
                "pending.write",
                "write the reviewed value",
                RiskTier::Mutate,
            )]
        }
        async fn run(
            &self,
            _: &str,
            input: serde_json::Value,
            _: &RunCtx<'_>,
        ) -> ark_tools::Result<ActionOutput> {
            self.0.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            Ok(ActionOutput::json(input))
        }
    }
    struct Together(Arc<tokio::sync::Barrier>);
    #[async_trait::async_trait]
    impl Approver for Together {
        async fn decide(&self, request: &crate::ApprovalRequest) -> ApprovalDecision {
            assert!(request.invocation.is_some());
            assert!(request.project.is_some());
            assert!(request.ask_each_time);
            self.0.wait().await;
            ApprovalDecision::AllowOnce
        }
    }
    fn engine(root: &std::path::Path, calls: Arc<AtomicUsize>) -> Engine {
        let mut registry = Registry::new();
        registry.register(Arc::new(Change(calls)));
        Engine::new(Store::open(root).unwrap(), registry).for_work("one-work")
    }
    #[tokio::test]
    async fn recovered_pending_call_is_claimed_once_and_completed_result_is_reused() {
        let root = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let original = engine(root.path(), calls.clone());
        let project = original.create_project("pending", "").unwrap();
        let pending = original
            .pending_approval(
                &project.id,
                "pending.write",
                serde_json::json!({"value": "exact"}),
            )
            .unwrap();
        drop(original);
        let restored = engine(root.path(), calls.clone());
        let gate = ApprovalGate::new(Arc::new(Together(Arc::new(tokio::sync::Barrier::new(2)))));
        let (first, second) = tokio::join!(
            restored.run_pending_with_approval(&project.id, &pending.id, &gate),
            restored.run_pending_with_approval(&project.id, &pending.id, &gate)
        );
        assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let gate = ApprovalGate::new(Arc::new(FixedApprover(ApprovalDecision::Deny)));
        let completed = restored
            .run_pending_with_approval(&project.id, &pending.id, &gate)
            .await
            .unwrap();
        assert_eq!(completed.status, InvocationStatus::Ok);
        assert_eq!(completed.output, serde_json::json!({"value": "exact"}));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn denied_running_changed_and_foreign_calls_never_execute() {
        let root = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let engine = engine(root.path(), calls.clone());
        let project = engine.create_project("pending", "").unwrap();
        let gate = ApprovalGate::new(Arc::new(FixedApprover(ApprovalDecision::Deny)));
        let mut pending = engine
            .pending_approval(&project.id, "pending.write", serde_json::json!({}))
            .unwrap();
        let denied = engine
            .run_pending_with_approval(&project.id, &pending.id, &gate)
            .await
            .unwrap();
        assert_eq!(denied.status, InvocationStatus::Denied);
        let allow = ApprovalGate::new(Arc::new(FixedApprover(ApprovalDecision::AllowOnce)));
        assert_eq!(
            engine
                .run_pending_with_approval(&project.id, &pending.id, &allow)
                .await
                .unwrap()
                .status,
            InvocationStatus::Denied
        );
        pending.id = InvocationId::new();
        pending.status = InvocationStatus::Running;
        engine
            .store
            .create_invocation(&project.id, &pending)
            .unwrap();
        assert!(engine
            .run_pending_with_approval(&project.id, &pending.id, &allow)
            .await
            .is_err());
        pending.id = InvocationId::new();
        pending.status = InvocationStatus::PendingApproval;
        pending.descriptor = Some("changed".into());
        engine
            .store
            .create_invocation(&project.id, &pending)
            .unwrap();
        assert!(engine
            .run_pending_with_approval(&project.id, &pending.id, &allow)
            .await
            .is_err());
        pending.id = InvocationId::new();
        pending.work = Some("another-work".into());
        engine
            .store
            .create_invocation(&project.id, &pending)
            .unwrap();
        assert!(engine
            .run_pending_with_approval(&project.id, &pending.id, &allow)
            .await
            .is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}
