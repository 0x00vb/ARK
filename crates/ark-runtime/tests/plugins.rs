//! MCP plugins added, used and removed while the engine runs, against a small stdio server
//! (`ark-tools/tests/fixtures/mcp_echo.py`). Skipped where `python3` is missing.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ark_core::InvocationStatus;
use ark_runtime::{Engine, PluginSpec, Plugins};
use ark_tools::{ActionOutput, Adapter, Registry, RunCtx};

fn server() -> Option<String> {
    let ok = std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !ok {
        eprintln!("python3 not found: skipped");
        return None;
    }
    let p =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../ark-tools/tests/fixtures/mcp_echo.py");
    Some(p.canonicalize().unwrap().display().to_string())
}

/// An app adapter named `files`, so a plugin may not take that name.
struct Files;
#[async_trait::async_trait]
impl Adapter for Files {
    fn name(&self) -> &str {
        "files"
    }
    fn actions(&self) -> Vec<ark_core::ActionDescriptor> {
        vec![ark_core::ActionDescriptor::new(
            "files.list",
            "list",
            ark_core::RiskTier::Read,
        )]
    }
    async fn run(
        &self,
        _a: &str,
        _i: serde_json::Value,
        _c: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        Ok(ActionOutput::json(serde_json::json!({})))
    }
}

fn names(r: &Registry) -> Vec<String> {
    let mut v: Vec<String> = r.actions().into_iter().map(|d| d.name).collect();
    v.sort();
    v
}

#[tokio::test]
async fn a_plugin_is_added_used_and_removed_while_the_engine_runs() {
    let Some(srv) = server() else { return };
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open_with(dir.path(), Registry::new().with(Arc::new(Files))).unwrap();
    assert!(engine.plugins().unwrap().list().is_empty());
    assert_eq!(names(&engine.tools()), ["files.list"]);

    // A name the app's tools use, or one that is not a name, is refused before anything starts.
    assert!(engine
        .add_plugin(PluginSpec::new("files", "python3").args([srv.clone()]))
        .is_err());
    assert!(engine
        .add_plugin(PluginSpec::new("Not OK", "python3").args([srv.clone()]))
        .is_err());

    let status = engine
        .add_plugin(PluginSpec::new("notes", "python3").args([srv.clone()]))
        .unwrap();
    assert!(status.running);
    assert_eq!(status.tools, ["notes.echo", "notes.stamp"]);
    assert_eq!(
        names(&engine.tools()),
        ["files.list", "notes.echo", "notes.stamp"]
    );
    assert!(engine
        .add_plugin(PluginSpec::new("notes", "python3").args([srv.clone()]))
        .is_err());

    // Saved to plugins.json.
    let file: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("plugins.json")).unwrap())
            .unwrap();
    assert_eq!(file["plugins"][0]["name"], "notes");

    // The root engine runs it; the risk comes from the server's annotations.
    let p = engine.create_project("p", "").unwrap();
    let inv = engine
        .run_action(
            &p.id,
            "notes.echo",
            serde_json::json!({ "text": "hi" }),
            false,
        )
        .await
        .unwrap();
    assert_eq!(inv.status, InvocationStatus::Ok, "{:?}", inv.error);
    assert_eq!(inv.output["text"], "hi");
    let inv = engine
        .run_action(&p.id, "notes.stamp", serde_json::json!({}), false)
        .await
        .unwrap();
    assert_eq!(inv.status, InvocationStatus::PendingApproval);

    // A turn takes the tools of its start and keeps them.
    let turn = engine.for_turn();
    engine.plugins().unwrap().remove("notes").unwrap();
    assert_eq!(names(&engine.tools()), ["files.list"]);
    assert_eq!(
        names(&turn.registry),
        ["files.list", "notes.echo", "notes.stamp"]
    );
    // A turn engine adds no plugins of its own (its registry is what it was given).
    assert_eq!(names(&turn.tools()), names(&turn.registry));
    let file: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("plugins.json")).unwrap())
            .unwrap();
    assert_eq!(file["plugins"], serde_json::json!([]));
}

#[tokio::test]
async fn plugins_json_is_read_at_start_and_a_broken_entry_says_why() {
    let Some(srv) = server() else { return };
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("plugins.json"),
        serde_json::json!({
            "keep": "other fields stay",
            "plugins": [
                { "name": "notes", "command": "python3", "args": [srv], "env": { "K": "v" } },
                { "name": "gone", "command": "/no/such/server" }
            ]
        })
        .to_string(),
    )
    .unwrap();
    let engine = Engine::open(dir.path()).unwrap();
    let list = engine.plugins().unwrap().list();
    assert_eq!(list.len(), 2);
    assert!(list[0].running);
    assert_eq!(list[0].env_keys, ["K"], "values stay out of the status");
    assert!(!list[1].running);
    assert!(list[1].error.as_deref().unwrap().contains("spawn"));
    assert_eq!(names(&engine.tools()), ["notes.echo", "notes.stamp"]);

    // A restart of the broken one fails again and says so; nothing else changes.
    let again = engine.plugins().unwrap().restart("gone").unwrap();
    assert!(!again.running && again.error.is_some());

    engine.plugins().unwrap().remove("gone").unwrap();
    let file: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("plugins.json")).unwrap())
            .unwrap();
    assert_eq!(file["keep"], "other fields stay");
    assert_eq!(file["plugins"].as_array().unwrap().len(), 1);
}

#[test]
fn a_server_that_never_answers_is_refused_within_the_limit() {
    let Some(srv) = server() else { return };
    let dir = tempfile::tempdir().unwrap();
    let plugins = Plugins::load(dir.path()).with_handshake_limit(Duration::from_millis(500));
    let start = Instant::now();
    let err = plugins
        .add(
            PluginSpec::new("quiet", "python3").args([srv, "--silent".into()]),
            &[],
        )
        .unwrap_err();
    assert!(err.to_string().contains("did not answer"), "{err}");
    assert!(start.elapsed() < Duration::from_secs(5));
    assert!(
        plugins.list().is_empty(),
        "a server that failed is not saved"
    );
    assert!(!dir.path().join("plugins.json").exists());
}
