//! The engine: ingest artifacts and run actions, recording every call as an invocation and
//! persisting everything it produces. This is where the approval gate is enforced: `Read`
//! and `Observe` run immediately; `Mutate` runs only with explicit approval. The agent loops
//! (`Agent`, `Harness`) are built on top of it.
//!
//! Unlike the source workstation, the engine registers **no** domain adapters — the consumer
//! supplies a [`Registry`]. Content classification is pluggable via [`Sniffer`]; ARK ships
//! only the text/json/binary classifier.

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
    pub store: Store,
    pub registry: Registry,
    sniffers: Vec<Arc<dyn Sniffer>>,
}

impl Engine {
    /// Construct with a store and a consumer-supplied registry.
    pub fn new(store: Store, registry: Registry) -> Self {
        Engine { store, registry, sniffers: Vec::new() }
    }

    /// Open a workspace at `root` with the given registry, then append any out-of-process MCP
    /// plugins declared in `<root>/plugins.json`.
    pub fn open_with(root: impl AsRef<std::path::Path>, mut registry: Registry) -> Result<Self, AgentError> {
        let root = root.as_ref().to_path_buf();
        load_plugins(&root, &mut registry);
        Ok(Engine::new(Store::open(root)?, registry))
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
    /// absent.
    pub fn workspace_dir(&self, project: &ProjectId) -> Result<std::path::PathBuf, AgentError> {
        let configured = self
            .store
            .get_project(project)?
            .and_then(|p| p.workspace)
            .map(std::path::PathBuf::from);
        let dir = configured
            .unwrap_or_else(|| self.store.root().join("workspaces").join(project.as_str()));
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
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
            Provenance::Ingested { path: path.display().to_string() },
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
                self.store.index_text(project, &art.id, &art.name, &kind.as_tag(), body);
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

        let blobs = StoreBlobs(&self.store);
        let ctx = RunCtx::new(&blobs).with_workspace(self.workspace_dir(project)?);

        match self.registry.run(action, input, &ctx).await {
            Ok(out) => {
                inv.command = out.command;
                for prod in out.produced {
                    let provenance = prod
                        .provenance
                        .unwrap_or_else(|| Provenance::Produced { invocation: inv.id.clone() });
                    // A Blob is re-sniffed so produced bytes land as their real kind.
                    let kind = if prod.kind == ArtifactKind::Blob {
                        self.detect_kind(&prod.name, &prod.bytes)
                    } else {
                        prod.kind
                    };
                    let art = self.ingest_bytes(project, &prod.name, kind, &prod.bytes, provenance)?;
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
    let plugins = doc.get("plugins").and_then(|p| p.as_array()).cloned().unwrap_or_default();
    for p in plugins {
        let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let command = p.get("command").and_then(|v| v.as_str()).unwrap_or("").to_string();
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
                        if pb.is_relative() && (s.contains('/') || s.ends_with(".py") || s.ends_with(".js")) {
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
            .map(|m| m.iter().filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string()))).collect())
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
