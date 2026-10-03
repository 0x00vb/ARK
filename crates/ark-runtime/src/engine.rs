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

pub struct Engine {
    /// Shared with every engine made by [`scoped`](Engine::scoped).
    pub store: Arc<Store>,
    pub registry: Registry,
    sniffers: Vec<Arc<dyn Sniffer>>,
}

impl Engine {
    /// Construct with a store and a consumer-supplied registry.
    pub fn new(store: Store, registry: Registry) -> Self {
        Engine {
            store: Arc::new(store),
            registry,
            sniffers: Vec::new(),
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
        }
    }

    /// Open a workspace at `root` with the given registry, then append any out-of-process MCP
    /// plugins declared in `<root>/plugins.json`.
    pub fn open_with(
        root: impl AsRef<std::path::Path>,
        mut registry: Registry,
    ) -> Result<Self, AgentError> {
        let root = root.as_ref().to_path_buf();
        load_plugins(&root, &mut registry);
        let engine = Engine::new(Store::open(root)?, registry);
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
        let desc = self
            .registry
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

        match self.registry.run(action, input, &ctx).await {
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

/// Load out-of-process MCP plugins declared in `<root>/plugins.json` and register each as an
/// adapter. Best-effort: a plugin that fails to connect is skipped with a warning so a broken
/// plugin never blocks startup. Schema:
/// `{ "plugins": [ { "name": "demo", "command": "python3", "args": ["srv.py"],
///    "env": { "K": "V" } } ] }`. Relative `args`/`command` resolve against `root`.
fn load_plugins(root: &std::path::Path, registry: &mut Registry) {
    let path = root.join("plugins.json");
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return, // no plugins file is the normal case
    };
    let doc: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("plugins.json is not valid JSON: {e}");
            return;
        }
    };
    let plugins = doc
        .get("plugins")
        .and_then(|p| p.as_array())
        .cloned()
        .unwrap_or_default();
    for p in plugins {
        let name = p
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let command = p
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if name.is_empty() || command.is_empty() {
            tracing::warn!("plugin entry missing name/command; skipped");
            continue;
        }
        let args: Vec<String> = p
            .get("args")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str())
                    .map(|s| {
                        let pb = std::path::Path::new(s);
                        if pb.is_relative()
                            && (s.contains('/') || s.ends_with(".py") || s.ends_with(".js"))
                        {
                            root.join(pb).display().to_string()
                        } else {
                            s.to_string()
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        let env: std::collections::HashMap<String, String> = p
            .get("env")
            .and_then(|v| v.as_object())
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default();

        match ark_tools::McpAdapter::connect(&name, &command, &args, &env) {
            Ok(adapter) => {
                tracing::info!("loaded MCP plugin `{name}`");
                registry.register(std::sync::Arc::new(adapter));
            }
            Err(e) => tracing::warn!("MCP plugin `{name}` failed to load: {e}"),
        }
    }
}
