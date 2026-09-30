//! # ark-tools
//!
//! The orchestration boundary. Every external tool a consumer wants the agent to use is
//! wrapped as an [`Adapter`] exposing typed [`ActionDescriptor`]s; the agent and any UI
//! invoke tools only through this. This crate depends on `ark-core` but on **no** other
//! ARK crate and on no storage — it is the seam that keeps tool-specific code isolated. A
//! tool's name appears in exactly one adapter, defined by the consumer.
//!
//! ARK ships **no domain adapters**. It ships the trait, the [`Registry`], a
//! [`subprocess`] helper for driving CLI tools, and the [`mcp`] bridge for out-of-process
//! plugins in any language. A security product registers `MachoAdapter`; a writing agent
//! registers a `DraftAdapter`; both use the exact same machinery.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use ark_core::{ActionDescriptor, ArtifactId, ArtifactKind};

pub mod mcp;
pub mod subprocess;

mod error;
pub use error::AdapterError;
pub use mcp::{McpAdapter, McpClient, McpTool};
pub type Result<T> = std::result::Result<T, AdapterError>;

/// How an adapter reads the bytes of an artifact it was asked to operate on. Implemented
/// by whoever owns the blob store (the engine), so adapters stay ignorant of the store.
pub trait BlobResolver: Send + Sync {
    /// A real filesystem path to the artifact's bytes, if one exists. Adapters that shell
    /// out to a tool hand this path to the tool.
    fn path(&self, id: &ArtifactId) -> Option<PathBuf>;
    /// The raw bytes of the artifact.
    fn read(&self, id: &ArtifactId) -> Result<Vec<u8>>;
}

/// Context handed to an action when it runs.
pub struct RunCtx<'a> {
    pub blobs: &'a dyn BlobResolver,
    /// The project's on-disk workspace root, if one is sited. Adapters that write files
    /// confine their operations to it; adapters that don't need it ignore it.
    pub workspace: Option<PathBuf>,
}

impl<'a> RunCtx<'a> {
    pub fn new(blobs: &'a dyn BlobResolver) -> Self {
        RunCtx { blobs, workspace: None }
    }
    pub fn with_workspace(mut self, workspace: PathBuf) -> Self {
        self.workspace = Some(workspace);
        self
    }
}

/// An artifact an action produced (a stdout capture, a generated file, …). The engine
/// stores the bytes and records provenance; adapters never touch storage.
pub struct ProducedArtifact {
    pub kind: ArtifactKind,
    pub name: String,
    pub bytes: Vec<u8>,
    /// Provenance override. Most produced artifacts are `Produced { invocation }` (the
    /// engine fills that in), but some carry `Captured` instead.
    pub provenance: Option<ark_core::Provenance>,
}

impl ProducedArtifact {
    pub fn text(name: impl Into<String>, body: impl Into<String>) -> Self {
        ProducedArtifact {
            kind: ArtifactKind::Text,
            name: name.into(),
            bytes: body.into().into_bytes(),
            provenance: None,
        }
    }

    pub fn bytes(kind: ArtifactKind, name: impl Into<String>, bytes: Vec<u8>) -> Self {
        ProducedArtifact { kind, name: name.into(), bytes, provenance: None }
    }

    pub fn with_provenance(mut self, p: ark_core::Provenance) -> Self {
        self.provenance = Some(p);
        self
    }
}

/// The structured result of running an action. This is what makes every tool call
/// observable and reproducible: a JSON summary, the exact command line, and any artifacts
/// produced.
#[derive(Default)]
pub struct ActionOutput {
    /// The structured result the agent reasons over and a UI renders.
    pub output: serde_json::Value,
    /// The concrete external command executed, verbatim, for reproducibility. `None` for
    /// in-process actions.
    pub command: Option<Vec<String>>,
    pub produced: Vec<ProducedArtifact>,
}

impl ActionOutput {
    pub fn json(output: serde_json::Value) -> Self {
        ActionOutput { output, command: None, produced: Vec::new() }
    }
    pub fn with_command(mut self, cmd: Vec<String>) -> Self {
        self.command = Some(cmd);
        self
    }
    pub fn with_artifact(mut self, a: ProducedArtifact) -> Self {
        self.produced.push(a);
        self
    }
}

/// A wrapped external capability. Implementors expose a set of actions and run them.
#[async_trait]
pub trait Adapter: Send + Sync {
    /// The adapter namespace (`fs`, `web`, `draft`…). Matches the prefix of every action
    /// name it offers.
    fn name(&self) -> &str;

    /// The actions this adapter offers, with their schemas and risk tiers.
    fn actions(&self) -> Vec<ActionDescriptor>;

    /// Whether the adapter's backing tool is actually available on this machine. The
    /// registry uses this to report a live tool list. Default: available.
    async fn health(&self) -> AdapterHealth {
        AdapterHealth::ok()
    }

    /// Run one action. `action` is the full namespaced name.
    async fn run(&self, action: &str, input: serde_json::Value, ctx: &RunCtx<'_>)
        -> Result<ActionOutput>;
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AdapterHealth {
    pub available: bool,
    pub detail: String,
}

impl AdapterHealth {
    pub fn ok() -> Self {
        AdapterHealth { available: true, detail: String::new() }
    }
    pub fn ok_with(detail: impl Into<String>) -> Self {
        AdapterHealth { available: true, detail: detail.into() }
    }
    pub fn missing(detail: impl Into<String>) -> Self {
        AdapterHealth { available: false, detail: detail.into() }
    }
}

/// The set of adapters available in a session. This is what the agent enumerates as its
/// tool list — one registry, one discovery mechanism for human and agent. ARK ships it
/// **empty**: the consumer registers the adapters their domain needs.
#[derive(Default, Clone)]
pub struct Registry {
    adapters: Vec<Arc<dyn Adapter>>,
}

impl Registry {
    pub fn new() -> Self {
        Registry { adapters: Vec::new() }
    }

    /// Build a registry from a list of adapters.
    pub fn from_adapters(adapters: impl IntoIterator<Item = Arc<dyn Adapter>>) -> Self {
        Registry { adapters: adapters.into_iter().collect() }
    }

    pub fn register(&mut self, a: Arc<dyn Adapter>) {
        self.adapters.push(a);
    }

    /// Builder form of [`register`](Registry::register).
    pub fn with(mut self, a: Arc<dyn Adapter>) -> Self {
        self.adapters.push(a);
        self
    }

    pub fn adapters(&self) -> &[Arc<dyn Adapter>] {
        &self.adapters
    }

    /// Every action across every adapter — the agent's tool list.
    pub fn actions(&self) -> Vec<ActionDescriptor> {
        self.adapters.iter().flat_map(|a| a.actions()).collect()
    }

    pub fn descriptor(&self, action: &str) -> Option<ActionDescriptor> {
        self.actions().into_iter().find(|d| d.name == action)
    }

    fn adapter_for(&self, action: &str) -> Option<Arc<dyn Adapter>> {
        let ns = action.split('.').next()?;
        self.adapters.iter().find(|a| a.name() == ns).cloned()
    }

    /// Run an action by full name.
    pub async fn run(
        &self,
        action: &str,
        input: serde_json::Value,
        ctx: &RunCtx<'_>,
    ) -> Result<ActionOutput> {
        let adapter = self
            .adapter_for(action)
            .ok_or_else(|| AdapterError::UnknownAction(action.to_string()))?;
        adapter.run(action, input, ctx).await
    }
}

// ---- shared input helpers (for adapter authors) ----------------------------

/// Resolve the single artifact-id argument shared by most actions that operate on one
/// stored object.
pub fn arg_artifact(action: &str, input: &serde_json::Value) -> Result<ArtifactId> {
    input
        .get("artifact")
        .and_then(|v| v.as_str())
        .map(|s| ArtifactId(s.to_string()))
        .ok_or_else(|| AdapterError::bad_input(action, "missing string field `artifact`"))
}

/// JSON schema shared by actions that take a single artifact id.
pub fn artifact_input_schema(desc: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "required": ["artifact"],
        "properties": {
            "artifact": { "type": "string", "description": desc }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_core::RiskTier;

    struct EchoAdapter;

    #[async_trait]
    impl Adapter for EchoAdapter {
        fn name(&self) -> &str {
            "echo"
        }
        fn actions(&self) -> Vec<ActionDescriptor> {
            vec![ActionDescriptor::new("echo.say", "echo the input text", RiskTier::Read)
                .with_input_schema(serde_json::json!({
                    "type": "object", "required": ["text"],
                    "properties": { "text": { "type": "string" } }
                }))]
        }
        async fn run(&self, _action: &str, input: serde_json::Value, _ctx: &RunCtx<'_>) -> Result<ActionOutput> {
            let text = input.get("text").and_then(|v| v.as_str()).unwrap_or("");
            Ok(ActionOutput::json(serde_json::json!({ "echoed": text })))
        }
    }

    struct NoBlobs;
    impl BlobResolver for NoBlobs {
        fn path(&self, _id: &ArtifactId) -> Option<PathBuf> {
            None
        }
        fn read(&self, id: &ArtifactId) -> Result<Vec<u8>> {
            Err(AdapterError::ArtifactNotFound(id.to_string()))
        }
    }

    #[tokio::test]
    async fn registry_registers_and_runs() {
        let reg = Registry::new().with(Arc::new(EchoAdapter));
        assert_eq!(reg.actions().len(), 1);
        assert!(reg.descriptor("echo.say").is_some());
        let blobs = NoBlobs;
        let ctx = RunCtx::new(&blobs);
        let out = reg.run("echo.say", serde_json::json!({ "text": "hi" }), &ctx).await.unwrap();
        assert_eq!(out.output["echoed"], "hi");
    }

    #[tokio::test]
    async fn unknown_action_errors() {
        let reg = Registry::new();
        let blobs = NoBlobs;
        let ctx = RunCtx::new(&blobs);
        assert!(reg.run("nope.nope", serde_json::json!({}), &ctx).await.is_err());
    }

    #[test]
    fn artifact_arg_helper() {
        let ok = serde_json::json!({ "artifact": "abc" });
        assert_eq!(arg_artifact("x.y", &ok).unwrap().as_str(), "abc");
        assert!(arg_artifact("x.y", &serde_json::json!({})).is_err());
    }
}
