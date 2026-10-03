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
use ark_tools::{Registry, RunCtx};

use crate::blobs::StoreBlobs;
use crate::error::AgentError;
use crate::plugins::{PluginSpec, PluginStatus, Plugins};

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
}

impl Engine {
    /// Construct with a store and a consumer-supplied registry.
    pub fn new(store: Store, registry: Registry) -> Self {
        Engine {
            store: Arc::new(store),
            registry,
            sniffers: Vec::new(),
            plugins: None,
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
        }
    }

    /// Everything this engine can run right now: its registry and the plugins connected at
    /// this moment. A turn or task takes this once, when it starts, and keeps it.
    pub fn tools(&self) -> Registry {
        let mut registry = self.registry.clone();
        if let Some(p) = &self.plugins {
            let own: Vec<String> = registry
                .adapters()
                .iter()
                .map(|a| a.name().to_string())
                .collect();
            for a in p.adapters() {
                if !own.iter().any(|n| n == a.name()) {
                    registry.register(a);
                }
            }
        }
        registry
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

    /// Mark calls left running by a process that no longer exists as failed, so nothing reads
    /// as still in progress (or, worse, as finished) after a crash. Returns how many.
    pub fn recover_interrupted(&self) -> Result<usize, AgentError> {
        let mut n = 0;
        for (project, mut inv) in self.store.list_running_invocations()? {
            if inv.pid.is_some_and(crate::process::alive) {
                continue;
            }
            inv.mark_failed("interrupted: the process stopped before this call finished");
            self.store.put_invocation(&project, &inv)?;
            n += 1;
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
        let id = self.store.put_blob(bytes)?;
        let art = Artifact::new(id, kind.clone(), name, bytes.len() as u64, provenance);
        self.store.put_artifact(project, &art)?;
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
        self.store.put_invocation(project, &inv)?;

        let blobs = StoreBlobs(&self.store);
        let ctx = RunCtx::new(&blobs).with_workspace(self.workspace_dir(project)?);

        match registry.run(action, input, &ctx).await {
            Ok(out) => {
                inv.command = out.command;
                for prod in out.produced {
                    let provenance = prod.provenance.unwrap_or_else(|| Provenance::Produced {
                        invocation: inv.id.clone(),
                    });
                    // A Blob is re-sniffed so produced bytes land as their real kind.
                    let kind = if prod.kind == ArtifactKind::Blob {
                        self.detect_kind(&prod.name, &prod.bytes)
                    } else {
                        prod.kind
                    };
                    let art =
                        self.ingest_bytes(project, &prod.name, kind, &prod.bytes, provenance)?;
                    inv.produced.push(art.id);
                }
                inv.mark_ok(out.output);
            }
            Err(e) => inv.mark_failed(e.to_string()),
        }

        self.store.put_invocation(project, &inv)?;
        Ok(inv)
    }
}

impl Engine {
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
