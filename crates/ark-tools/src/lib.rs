//! # ark-tools
//!
//! The orchestration boundary. Every external tool a consumer wants the agent to use is
//! wrapped as an [`Adapter`] exposing typed [`ActionDescriptor`]s; the agent and any UI
//! invoke tools only through this. This crate depends on `ark-core` but on **no** other
//! ARK crate and on no storage — it is the seam that keeps tool-specific code isolated. A
//! tool's name appears in exactly one adapter, defined by the consumer.
//!
//! ARK ships **no domain adapters**. It ships the trait, the [`Registry`], a
//! [`subprocess`] helper for driving CLI tools, the [`mcp`] bridge for out-of-process
//! plugins in any language, and two adapters any domain can use, [`diagram`] and [`web`]
//! (both off unless the app turns them on). A security product registers `MachoAdapter`; a writing agent
//! registers a `DraftAdapter`; both use the exact same machinery.

use std::path::PathBuf;
use std::sync::Arc;

use ark_core::{ActionDescriptor, ArtifactId, ArtifactKind};
use async_trait::async_trait;

pub mod diagram;
pub mod mcp;
pub mod subprocess;
pub mod web;

mod error;
pub use error::AdapterError;
pub use mcp::{McpAdapter, McpClient, McpTool};
pub type Result<T> = std::result::Result<T, AdapterError>;

/// A durable consumer transition invoked when a turn stops or finishes.
pub type TurnLifecycleCallback = Arc<dyn Fn() -> std::result::Result<(), String> + Send + Sync>;

/// One consumer's idempotent lifecycle for a turn. Reuse the same `Arc` for retries.
pub struct TurnLifecycle {
    pub key: String,
    pub initialize: TurnLifecycleCallback,
    pub on_stop: TurnLifecycleCallback,
    pub on_finish: TurnLifecycleCallback,
}

/// Serializes a short synchronous commit with cancellation acknowledgement.
pub trait CommitFence: Send + Sync {
    /// Trusted opaque identity for this turn.
    fn turn_id(&self) -> &str;
    /// Registers a lifecycle under the fence. This may block and must run off async workers.
    /// Failed initialization stays registered for stop or finish reconciliation.
    fn register_lifecycle(
        &self,
        lifecycle: Arc<TurnLifecycle>,
    ) -> std::result::Result<bool, String>;

    /// Runs `commit` only if cancellation has not won. A cancellation acknowledgement must
    /// drain the fence. The callback must not await or call cancellation.
    fn if_active(&self, commit: &mut dyn FnMut()) -> bool;
}

/// One-way runtime signal that an adapter is attempting an atomic publication.
#[derive(Default)]
pub struct AtomicPublicationIntent(std::sync::atomic::AtomicBool);

impl AtomicPublicationIntent {
    pub fn requested(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Acquire)
    }
}

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
    /// The work this call is made for (a conversation's turn), when the caller names one. An
    /// adapter that creates records of its own (a task) can keep it on them.
    pub work: Option<String>,
    /// Earlier work this call's work continues: runs of it that stopped before they finished.
    /// An adapter can take up a record they left open instead of making another.
    pub continues: Vec<String>,
    /// The durable invocation currently executing. Adapters use this opaque id for external
    /// idempotency. They must not send a chat or work id to a provider.
    pub invocation: Option<String>,
    /// The project selected by the runtime, never supplied by tool input.
    pub project: Option<String>,
    /// Turn cancellation fence, when this invocation belongs to a cancellable turn.
    pub commit_fence: Option<Arc<dyn CommitFence>>,
    /// Runtime-owned marker; adapters declare intent through `request_atomic_publication`.
    pub atomic_publication: Option<Arc<AtomicPublicationIntent>>,
}

impl<'a> RunCtx<'a> {
    pub fn new(blobs: &'a dyn BlobResolver) -> Self {
        RunCtx {
            blobs,
            workspace: None,
            work: None,
            continues: Vec::new(),
            invocation: None,
            project: None,
            commit_fence: None,
            atomic_publication: None,
        }
    }
    pub fn with_workspace(mut self, workspace: PathBuf) -> Self {
        self.workspace = Some(workspace);
        self
    }
    pub fn with_work(mut self, work: Option<String>, continues: Vec<String>) -> Self {
        self.work = work;
        self.continues = continues;
        self
    }
    /// Name the durable invocation which owns this call.
    pub fn with_invocation(mut self, invocation: impl Into<String>) -> Self {
        self.invocation = Some(invocation.into());
        self
    }
    pub fn with_project(mut self, project: impl Into<String>) -> Self {
        self.project = Some(project.into());
        self
    }
    pub fn with_commit_fence(mut self, fence: Option<Arc<dyn CommitFence>>) -> Self {
        self.commit_fence = fence;
        self
    }
    /// Install the runtime-owned marker for a call that may publish atomically.
    pub fn with_atomic_publication(mut self, marker: Arc<AtomicPublicationIntent>) -> Self {
        self.atomic_publication = Some(marker);
        self
    }
    /// Declare publication intent before attempting any durable output commit.
    pub fn request_atomic_publication(&self) -> Result<()> {
        let marker = self
            .atomic_publication
            .as_ref()
            .ok_or_else(|| AdapterError::Tool("Atomic publication is unavailable".into()))?;
        marker.0.store(true, std::sync::atomic::Ordering::Release);
        Ok(())
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
    /// The file in the project folder these bytes were written to. The stored artifact is a
    /// snapshot; the file is what a person opens and what later calls change.
    pub path: Option<String>,
    /// The bytes are the file at `path` as it was before this call changed or removed it, kept
    /// so the person can put it back (ADR 0034). Not a file the call wrote.
    pub before: bool,
}

impl ProducedArtifact {
    pub fn text(name: impl Into<String>, body: impl Into<String>) -> Self {
        ProducedArtifact {
            kind: ArtifactKind::Text,
            name: name.into(),
            bytes: body.into().into_bytes(),
            provenance: None,
            path: None,
            before: false,
        }
    }

    pub fn bytes(kind: ArtifactKind, name: impl Into<String>, bytes: Vec<u8>) -> Self {
        ProducedArtifact {
            kind,
            name: name.into(),
            bytes,
            provenance: None,
            path: None,
            before: false,
        }
    }

    pub fn with_provenance(mut self, p: ark_core::Provenance) -> Self {
        self.provenance = Some(p);
        self
    }

    /// The bytes are the file at `path` in the project folder (relative, with `/`).
    pub fn at(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    /// The bytes are the file at `path` as it was before the call changed or removed it.
    pub fn before(kind: ArtifactKind, path: impl Into<String>, bytes: Vec<u8>) -> Self {
        let path = path.into();
        let name = path.rsplit('/').next().unwrap_or(&path).to_string();
        let mut a = ProducedArtifact::bytes(kind, name, bytes).at(path);
        a.before = true;
        a
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
    /// Pictures shown to the model with this result (a window, a page), when it reads images.
    /// Kept in memory for the next request only, never stored.
    pub pictures: Vec<Picture>,
    /// A trusted external-effect outcome. The runtime persists this separately from `output`.
    pub effect: Option<ark_core::ExternalEffect>,
}

/// An image a model sees, as its bytes and media type (`image/png`).
#[derive(Clone, Debug, PartialEq)]
pub struct Picture {
    pub media_type: String,
    pub bytes: std::sync::Arc<Vec<u8>>,
}

impl ActionOutput {
    pub fn json(output: serde_json::Value) -> Self {
        ActionOutput {
            output,
            command: None,
            produced: Vec::new(),
            pictures: Vec::new(),
            effect: None,
        }
    }
    /// Show the model a picture with this result ([`ActionOutput::pictures`]).
    pub fn with_image(mut self, media_type: &str, bytes: Vec<u8>) -> Self {
        self.pictures.push(Picture {
            media_type: media_type.to_string(),
            bytes: std::sync::Arc::new(bytes),
        });
        self
    }
    pub fn with_command(mut self, cmd: Vec<String>) -> Self {
        self.command = Some(cmd);
        self
    }
    pub fn with_artifact(mut self, a: ProducedArtifact) -> Self {
        self.produced.push(a);
        self
    }
    pub fn with_effect(mut self, effect: ark_core::ExternalEffect) -> Self {
        self.effect = Some(effect);
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
    async fn run(
        &self,
        action: &str,
        input: serde_json::Value,
        ctx: &RunCtx<'_>,
    ) -> Result<ActionOutput>;

    /// For a model continuing `works` (runs that stopped before they finished, see
    /// [`RunCtx::continues`]): one line per record this adapter keeps from them that the model
    /// should know about (work handed off and still under way). Default: none.
    fn continued(&self, _works: &[String]) -> Vec<String> {
        Vec::new()
    }

    /// A few words on what its actions are for, shown beside their names in the tool index a
    /// model reads before it loads any schema ("run a project's commands: tests, builds").
    /// Default: none, the names alone.
    fn summary(&self) -> Option<&str> {
        None
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AdapterHealth {
    pub available: bool,
    pub detail: String,
}

impl AdapterHealth {
    pub fn ok() -> Self {
        AdapterHealth {
            available: true,
            detail: String::new(),
        }
    }
    pub fn ok_with(detail: impl Into<String>) -> Self {
        AdapterHealth {
            available: true,
            detail: detail.into(),
        }
    }
    pub fn missing(detail: impl Into<String>) -> Self {
        AdapterHealth {
            available: false,
            detail: detail.into(),
        }
    }
}

/// Adapters that come and go while ARK runs: MCP plugins, connected accounts. The engine asks
/// every source when a turn starts and keeps the answer for that turn.
pub trait ToolSource: Send + Sync {
    /// The adapters available right now.
    fn adapters(&self) -> Vec<Arc<dyn Adapter>>;
    /// The adapters available for this project at the start of a turn.
    fn adapters_for(&self, _project: &str) -> Vec<Arc<dyn Adapter>> {
        self.adapters()
    }
    /// Names reserved even when no project currently offers an adapter.
    fn reserved_names(&self) -> Vec<String> {
        Vec::new()
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
        Registry {
            adapters: Vec::new(),
        }
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

    /// A view of this registry with only the actions `keep` accepts. The adapters are shared,
    /// not copied; an action left out is neither listed nor run. Gives each agent of an
    /// orchestration its own tool permissions over one set of adapters.
    pub fn filtered(&self, keep: impl Fn(&str) -> bool) -> Registry {
        let mut out = Registry::new();
        for a in &self.adapters {
            let allowed: Vec<String> = a
                .actions()
                .into_iter()
                .map(|d| d.name)
                .filter(|n| keep(n))
                .collect();
            if allowed.is_empty() {
                continue;
            }
            if allowed.len() == a.actions().len() {
                out.register(a.clone());
            } else {
                out.register(Arc::new(Filtered {
                    inner: a.clone(),
                    allowed,
                }));
            }
        }
        out
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
        // A tool that panics fails its own call; the turn goes on and the model sees the failure.
        futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(
            adapter.run(action, input, ctx),
        ))
        .await
        .unwrap_or_else(|panic| {
            let why = panic
                .downcast_ref::<&str>()
                .map(|s| (*s).to_string())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown cause".into());
            Err(AdapterError::Tool(format!("`{action}` crashed: {why}")))
        })
    }
}

/// An adapter with some of its actions hidden (see [`Registry::filtered`]).
struct Filtered {
    inner: Arc<dyn Adapter>,
    allowed: Vec<String>,
}

#[async_trait]
impl Adapter for Filtered {
    fn name(&self) -> &str {
        self.inner.name()
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        self.inner
            .actions()
            .into_iter()
            .filter(|d| self.allowed.contains(&d.name))
            .collect()
    }
    async fn health(&self) -> AdapterHealth {
        self.inner.health().await
    }
    async fn run(
        &self,
        action: &str,
        input: serde_json::Value,
        ctx: &RunCtx<'_>,
    ) -> Result<ActionOutput> {
        if !self.allowed.iter().any(|a| a == action) {
            return Err(AdapterError::UnknownAction(action.to_string()));
        }
        self.inner.run(action, input, ctx).await
    }
}

/// Whether `action` matches a pattern: an exact name (`fs.read`), a namespace (`fs.*`) or
/// everything (`*`).
pub fn action_matches(pattern: &str, action: &str) -> bool {
    let p = pattern.trim();
    p == "*"
        || p == action
        || p.strip_suffix(".*")
            .is_some_and(|ns| action.split('.').next() == Some(ns))
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
            vec![
                ActionDescriptor::new("echo.say", "echo the input text", RiskTier::Read)
                    .with_input_schema(serde_json::json!({
                        "type": "object", "required": ["text"],
                        "properties": { "text": { "type": "string" } }
                    })),
            ]
        }
        async fn run(
            &self,
            _action: &str,
            input: serde_json::Value,
            _ctx: &RunCtx<'_>,
        ) -> Result<ActionOutput> {
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
        let out = reg
            .run("echo.say", serde_json::json!({ "text": "hi" }), &ctx)
            .await
            .unwrap();
        assert_eq!(out.output["echoed"], "hi");
    }

    struct Crashes;

    #[async_trait]
    impl Adapter for Crashes {
        fn name(&self) -> &str {
            "crash"
        }
        fn actions(&self) -> Vec<ActionDescriptor> {
            vec![ActionDescriptor::new("crash.now", "panics", RiskTier::Read)]
        }
        async fn run(
            &self,
            _action: &str,
            _input: serde_json::Value,
            _ctx: &RunCtx<'_>,
        ) -> Result<ActionOutput> {
            panic!("byte range starts at 1 but ends at 0");
        }
    }

    #[tokio::test]
    async fn an_action_that_panics_returns_an_error() {
        let reg = Registry::new().with(Arc::new(Crashes));
        let blobs = NoBlobs;
        let ctx = RunCtx::new(&blobs);
        let err = reg
            .run("crash.now", serde_json::json!({}), &ctx)
            .await
            .err()
            .expect("a crash is an error")
            .to_string();
        assert!(err.contains("`crash.now` crashed: byte range"), "{err}");
    }

    #[tokio::test]
    async fn unknown_action_errors() {
        let reg = Registry::new();
        let blobs = NoBlobs;
        let ctx = RunCtx::new(&blobs);
        assert!(reg
            .run("nope.nope", serde_json::json!({}), &ctx)
            .await
            .is_err());
    }

    struct Two;
    #[async_trait]
    impl Adapter for Two {
        fn name(&self) -> &str {
            "two"
        }
        fn actions(&self) -> Vec<ActionDescriptor> {
            vec![
                ActionDescriptor::new("two.read", "r", RiskTier::Read),
                ActionDescriptor::new("two.write", "w", RiskTier::Mutate),
            ]
        }
        async fn run(
            &self,
            a: &str,
            _i: serde_json::Value,
            _c: &RunCtx<'_>,
        ) -> Result<ActionOutput> {
            Ok(ActionOutput::json(serde_json::json!({ "ran": a })))
        }
    }

    #[tokio::test]
    async fn a_filtered_view_hides_and_refuses_actions() {
        let reg = Registry::new()
            .with(Arc::new(Two))
            .with(Arc::new(EchoAdapter));
        let view = reg.filtered(|a| action_matches("two.read", a));
        let names: Vec<String> = view.actions().into_iter().map(|d| d.name).collect();
        assert_eq!(names, vec!["two.read"]);
        let blobs = NoBlobs;
        let ctx = RunCtx::new(&blobs);
        assert!(view
            .run("two.read", serde_json::json!({}), &ctx)
            .await
            .is_ok());
        assert!(view
            .run("two.write", serde_json::json!({}), &ctx)
            .await
            .is_err());
        assert!(view
            .run("echo.say", serde_json::json!({}), &ctx)
            .await
            .is_err());
        assert_eq!(reg.filtered(|a| action_matches("*", a)).actions().len(), 3);
        assert!(action_matches("two.*", "two.write"));
        assert!(!action_matches("two.*", "twofold.x"));
    }

    #[test]
    fn artifact_arg_helper() {
        let ok = serde_json::json!({ "artifact": "abc" });
        assert_eq!(arg_artifact("x.y", &ok).unwrap().as_str(), "abc");
        assert!(arg_artifact("x.y", &serde_json::json!({})).is_err());
    }
}

pub mod events;
