use ark_core::{ActionDescriptor, RiskTier};
use ark_runtime::Engine;
use ark_tools::{ActionOutput, Adapter, RunCtx, ToolSource};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

struct ScopedSource {
    enabled: Arc<Mutex<BTreeSet<String>>>,
}
struct FixtureAdapter;
#[async_trait::async_trait]
impl Adapter for FixtureAdapter {
    fn name(&self) -> &str {
        "fixture_design"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![ActionDescriptor::new(
            "fixture_design.read",
            "Read",
            RiskTier::Read,
        )]
    }
    async fn run(&self, _: &str, _: Value, _: &RunCtx<'_>) -> ark_tools::Result<ActionOutput> {
        Ok(ActionOutput::json(json!({})))
    }
}
impl ToolSource for ScopedSource {
    fn adapters(&self) -> Vec<Arc<dyn Adapter>> {
        vec![]
    }
    fn reserved_names(&self) -> Vec<String> {
        vec!["fixture_design".into()]
    }

    fn adapters_for(&self, project: &str) -> Vec<Arc<dyn Adapter>> {
        if self.enabled.lock().unwrap().contains(project) {
            vec![Arc::new(FixtureAdapter)]
        } else {
            vec![]
        }
    }
}
#[tokio::test]
async fn project_catalog_snapshots_are_isolated_and_change_on_next_turn() {
    let root = tempfile::tempdir().unwrap();
    let mut engine = Engine::open(root.path()).unwrap();
    let a = engine.create_project("first", "").unwrap().id;
    let b = engine.create_project("second", "").unwrap().id;
    let enabled = Arc::new(Mutex::new(BTreeSet::from([a.as_str().to_owned()])));
    engine
        .try_add_source(Arc::new(ScopedSource {
            enabled: enabled.clone(),
        }))
        .unwrap();
    assert!(engine
        .add_plugin(ark_runtime::PluginSpec::new("fixture_design", "unused"))
        .is_err());
    let first = engine.for_project_turn(&a);
    assert!(first.registry.descriptor("fixture_design.read").is_some());
    assert!(first
        .run_action(&a, "fixture_design.read", json!({}), false)
        .await
        .is_ok());
    assert!(matches!(
        first
            .run_action(&b, "fixture_design.read", json!({}), false)
            .await,
        Err(ark_runtime::AgentError::ProjectScopeMismatch)
    ));
    assert!(first.tools_for(&b).actions().is_empty());
    let rebound = first.scoped_for(&b, first.registry.clone());
    assert!(matches!(
        rebound
            .run_action(&b, "fixture_design.read", json!({}), false)
            .await,
        Err(ark_runtime::AgentError::ProjectScopeMismatch)
    ));

    assert!(engine
        .for_project_turn(&b)
        .registry
        .descriptor("fixture_design.read")
        .is_none());
    assert!(engine.tools().descriptor("fixture_design.read").is_none());
    enabled.lock().unwrap().clear();
    assert!(first.registry.descriptor("fixture_design.read").is_some());
    assert!(engine
        .for_project_turn(&a)
        .registry
        .descriptor("fixture_design.read")
        .is_none());
}

#[test]
fn preexisting_plugin_blocks_reserved_source_without_removing_plugin() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("plugins.json"),
        r#"{"plugins":[{"name":"fixture_design","command":"/nonexistent-ark-design-fixture"}]}"#,
    )
    .unwrap();
    let mut engine = Engine::open(root.path()).unwrap();
    let source = ScopedSource {
        enabled: Arc::new(Mutex::new(BTreeSet::new())),
    };
    assert!(engine.try_add_source(Arc::new(source)).is_err());
    assert_eq!(engine.plugins().unwrap().list()[0].name, "fixture_design");
}

struct DormantSource(Arc<std::sync::atomic::AtomicBool>);
struct ExistingAdapter;
#[async_trait::async_trait]
impl Adapter for ExistingAdapter {
    fn name(&self) -> &str {
        "fixture_design"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![ActionDescriptor::new(
            "fixture_design.read",
            "Existing",
            RiskTier::Read,
        )]
    }
    async fn run(&self, _: &str, _: Value, _: &RunCtx<'_>) -> ark_tools::Result<ActionOutput> {
        Ok(ActionOutput::json(json!({})))
    }
}
impl ToolSource for DormantSource {
    fn adapters(&self) -> Vec<Arc<dyn Adapter>> {
        if self.0.load(std::sync::atomic::Ordering::SeqCst) {
            vec![Arc::new(ExistingAdapter)]
        } else {
            vec![]
        }
    }
}
#[test]
fn dormant_source_collision_keeps_existing_adapter_and_reports_unavailable() {
    let root = tempfile::tempdir().unwrap();
    let mut engine = Engine::open(root.path()).unwrap();
    let project = engine.create_project("project", "").unwrap().id;
    let active = Arc::new(std::sync::atomic::AtomicBool::new(false));
    engine.add_source(Arc::new(DormantSource(active.clone())));
    let enabled = Arc::new(Mutex::new(BTreeSet::from([project.as_str().to_owned()])));
    engine
        .try_add_source(Arc::new(ScopedSource {
            enabled: enabled.clone(),
        }))
        .unwrap();
    assert!(engine.try_register(Arc::new(ExistingAdapter)).is_err());
    assert!(engine
        .tools_for(&project)
        .descriptor("fixture_design.read")
        .is_some());
    active.store(true, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        engine.source_conflicts_for(&project),
        vec!["fixture_design"]
    );
    let tools = engine.tools_for(&project);
    assert!(tools.descriptor("fixture_design.read").is_none());
    enabled.lock().unwrap().clear();
    assert_eq!(
        engine.source_conflicts_for(&project),
        vec!["fixture_design"]
    );
    assert!(engine
        .tools_for(&project)
        .descriptor("fixture_design.read")
        .is_none());
    enabled.lock().unwrap().insert(project.as_str().to_owned());
    active.store(false, std::sync::atomic::Ordering::SeqCst);
    assert!(engine.source_conflicts_for(&project).is_empty());
    assert!(engine
        .tools_for(&project)
        .descriptor("fixture_design.read")
        .is_some());
}
