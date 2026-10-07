//! ARK's own tools in Codex's reply schema (ADR 0049): every tool becomes a closed variant in
//! OpenAI's strict form, and none of ARK's tools needs a part sent as a JSON string.
//!
//! `ARK_REPLY_SCHEMA_OUT=<file>` writes the schema of all of them, for a live check with
//! `codex exec --output-schema <file>`.

use std::sync::Arc;

use ark_model::cli::{codex_reply_schema, strict_problems};
use ark_model::ToolDef;
use ark_runtime::catalog::{action_tooldef, meta_tool_defs, MetaToolConfig};
use ark_tools::Adapter;
use serde_json::Value;

fn adapters(dir: &std::path::Path) -> Vec<Arc<dyn Adapter>> {
    let mut a: Vec<Arc<dyn Adapter>> = vec![
        Arc::new(ark_files::Files),
        Arc::new(ark_tools::diagram::Diagrams),
        Arc::new(ark_tools::web::WebAdapter::new()),
        Arc::new(ark_runtime::RulesTool::new(Vec::new())),
        Arc::new(ark_runtime::SkillsTool::new(Vec::new())),
        Arc::new(ark_browser::BrowserService::new(dir.join("browser")).for_owner("t")),
        Arc::new(
            ark_computer::ComputerService::with_screen(Arc::new(
                ark_computer::fake::FakeScreen::new(),
            ))
            .for_owner("t"),
        ),
    ];
    if let Some(p) = ark_proc::Processes::open(dir.join("proc")) {
        a.push(Arc::new(ark_proc::ProcessTool(p)));
    }
    let python: Arc<dyn ark_exec::Runtime> = Arc::new(ark_exec::Python::new(dir.join("python")));
    if let Some(e) = ark_exec::Exec::new(dir.join("exec"), vec![python]) {
        a.push(Arc::new(e));
    }
    let conns = ark_connect::Connections::open(
        &dir.join("conn"),
        ark_connect::Catalog::builtin(),
        Arc::new(ark_connect::MemorySecrets::default()),
    );
    for s in ["google.mail", "google.calendar"] {
        a.extend(ark_connect::services::adapter(
            s,
            conns.clone(),
            "a@example.com".into(),
        ));
    }
    a
}

fn all_tools() -> Vec<ToolDef> {
    let dir = tempfile::tempdir().unwrap();
    let mut tools: Vec<ToolDef> = adapters(dir.path())
        .iter()
        .flat_map(|a| a.actions())
        .map(|d| action_tooldef(&d))
        .collect();
    tools.extend(meta_tool_defs(MetaToolConfig {
        knowledge: true,
        acquisition: true,
        present: true,
    }));
    tools
}

#[test]
fn every_ark_tool_is_a_typed_variant_with_no_part_as_text() {
    let tools = all_tools();
    assert!(tools.len() > 40, "{} tools", tools.len());
    let schema = codex_reply_schema(&tools);
    assert_eq!(strict_problems(&schema), Vec::<String>::new());
    let variants = schema["properties"]["tool_calls"]["items"]["anyOf"]
        .as_array()
        .expect("one variant per tool");
    assert_eq!(variants.len(), tools.len());
    let mut as_text = Vec::new();
    for v in variants {
        let name = v["properties"]["name"]["enum"][0].as_str().unwrap();
        strings_of_json(&v["properties"]["arguments"], name, &mut as_text);
    }
    assert_eq!(as_text, Vec::<String>::new(), "parts sent as JSON strings");
    if let Ok(out) = std::env::var("ARK_REPLY_SCHEMA_OUT") {
        std::fs::write(out, schema.to_string()).unwrap();
    }
}

/// Where a part of the arguments fell back to JSON written as a string.
fn strings_of_json(v: &Value, at: &str, out: &mut Vec<String>) {
    if v["type"] == "string" && v["description"] == "JSON, written as a string" {
        out.push(at.to_string());
    }
    if let Some(p) = v["properties"].as_object() {
        for (k, x) in p {
            strings_of_json(x, &format!("{at}.{k}"), out);
        }
    }
    if v.get("items").is_some() {
        strings_of_json(&v["items"], &format!("{at}[]"), out);
    }
    for b in v["anyOf"].as_array().into_iter().flatten() {
        strings_of_json(b, at, out);
    }
}
