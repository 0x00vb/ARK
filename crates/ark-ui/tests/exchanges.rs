//! The window calls that read what agents said to each other (ADR 0119), through a real Host
//! and records in its store.

use std::sync::Arc;

use ark_interact::ark_orchestrate::Orchestrator;
use ark_interact::App;
use ark_model::{Completion, Message, Provider, ToolDef, Usage};
use ark_sdk::prelude::*;
use ark_ui::{ArkUi, Host};
use serde_json::{json, Value};

struct Quiet;

#[async_trait::async_trait]
impl Provider for Quiet {
    async fn complete(
        &self,
        _: &str,
        _: &[Message],
        _: &[ToolDef],
    ) -> ark_model::Result<Completion> {
        Ok(Completion {
            content: "ok".into(),
            tool_calls: vec![],
            usage: Usage::default(),
        })
    }
    fn describe(&self) -> String {
        "quiet".into()
    }
}

async fn open(dir: &std::path::Path) -> (Arc<Host>, Arc<ark_store::Store>) {
    let ark = Ark::builder()
        .store_root(dir)
        .provider(Arc::new(Quiet))
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let store = ark.shared_engine().store.clone();
    let app = App::new("Test").ark(ark).orchestrate(|ark, project, gate| {
        Ok(Orchestrator::builder(ark, project)
            .approvals(gate)
            .build()?)
    });
    (ArkUi::new(app).open().await.unwrap().0, store)
}

#[tokio::test]
async fn the_window_reads_lines_and_a_talk_and_refuses_an_unknown_project() {
    let dir = tempfile::tempdir().unwrap();
    let (host, store) = open(dir.path()).await;
    let summary = host.call("summary", json!({})).await.unwrap();
    let project = summary["project"]["id"].as_str().unwrap().to_string();
    for (name, title) in [("ceo", ""), ("cmo", "CMO")] {
        host.call(
            "resource",
            json!({
                "ref": { "location": summary["location"], "kind": "project", "id": "" },
                "operation": { "kind": "spawn_agent", "input": { "spec": { "name": name, "title": title, "look": "owl" } } },
            }),
        )
        .await
        .unwrap();
    }
    let none = host
        .call(
            "exchange_lines",
            json!({ "project": project, "agent": "ceo" }),
        )
        .await
        .unwrap();
    assert_eq!(none, json!([]));
    store
        .put_record(
            "orch_task",
            "t-brief",
            &project,
            &json!({
                "id": "t-brief", "project": project, "owner": "cmo", "objective": "write the copy",
                "origin": { "by": "agent", "agent": "ceo", "task": "t-turn" },
                "state": "done", "result": "copy ready", "created_ms": 10, "finished_ms": 20
            }),
        )
        .unwrap();
    store
        .put_record(
            "orch_message",
            "00000000000030-a",
            &project,
            &json!({ "from": "ceo", "to": "cmo", "task": "t-turn", "at_ms": 30, "text": "and be brief" }),
        )
        .unwrap();
    let lines = host
        .call(
            "exchange_lines",
            json!({ "project": project, "agent": "ceo", "session": "" }),
        )
        .await
        .unwrap();
    assert_eq!(lines.as_array().unwrap().len(), 1, "{lines}");
    let line = &lines[0];
    assert_eq!(line["turn"], "t-turn");
    assert_eq!(line["count"], 3);
    assert_eq!(line["label"], "3 messages with CMO");
    assert_eq!(line["with"][0]["agent"], "cmo");
    assert_eq!(line["with"][0]["look"], "owl");
    assert_eq!(
        line["parts"],
        json!([{ "text": "3 messages with " }, { "with": 0 }])
    );
    let talk = host
        .call(
            "exchange_talk",
            json!({ "project": project, "a": "ceo", "b": "cmo" }),
        )
        .await
        .unwrap();
    let said: Vec<(&str, &str)> = talk["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| (m["author"].as_str().unwrap(), m["text"].as_str().unwrap()))
        .collect();
    assert_eq!(
        said,
        vec![
            ("ceo", "write the copy"),
            ("cmo", "copy ready"),
            ("ceo", "and be brief")
        ]
    );
    assert_eq!(talk["with"][0]["name"], "ceo");
    assert_eq!(talk["with"][1]["name"], "CMO");
    assert!(
        talk["messages"][0].get("failed").is_none(),
        "a failed flag is only sent when set"
    );
    for call in [
        (
            "exchange_lines",
            json!({ "project": "missing", "agent": "ceo" }),
        ),
        (
            "exchange_talk",
            json!({ "project": "missing", "a": "ceo", "b": "cmo" }),
        ),
        (
            "exchange_talk",
            json!({ "project": project, "a": "ceo", "b": "nobody" }),
        ),
    ] {
        assert!(host.call(call.0, call.1).await.is_err(), "{}", call.0);
    }
    let _: Value = host.call("summary", json!({})).await.unwrap();
}
