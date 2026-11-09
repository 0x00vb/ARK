//! Group chats through a real Host and a scripted model (ADR 0113).

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ark_interact::ark_orchestrate::Orchestrator;
use ark_interact::App;
use ark_model::{Completion, Message, Provider, ProviderError, ToolDef, Usage};
use ark_sdk::prelude::*;
use ark_ui::{ArkUi, Host};
use serde_json::{json, Value};

/// Answers by who is asking: the router, or the agent the context calls "(you)".
#[derive(Default)]
struct Script {
    router_calls: AtomicUsize,
    router_answer: Mutex<String>,
    replies: Mutex<HashMap<String, String>>,
    failing: Mutex<Vec<String>>,
    asked: Mutex<Vec<String>>,
    seen: Mutex<Vec<(String, String)>>,
}

struct Model(Arc<Script>);

#[async_trait::async_trait]
impl Provider for Model {
    async fn complete(
        &self,
        system: &str,
        messages: &[Message],
        _tools: &[ToolDef],
    ) -> ark_model::Result<Completion> {
        let s = &self.0;
        let text = |content: String| Completion {
            content,
            tool_calls: vec![],
            usage: Usage::default(),
        };
        if system.starts_with("You route") {
            s.router_calls.fetch_add(1, Ordering::SeqCst);
            return Ok(text(s.router_answer.lock().unwrap().clone()));
        }
        let all = format!(
            "{system}\n{}",
            messages
                .iter()
                .map(|m| m.content.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        );
        let who = ["Scout", "Ana"]
            .into_iter()
            .find(|n| all.contains(&format!("- {n} (you)")))
            .unwrap_or("?");
        s.asked.lock().unwrap().push(who.to_string());
        s.seen.lock().unwrap().push((who.to_string(), all.clone()));
        if s.failing.lock().unwrap().iter().any(|n| n == who) {
            return Err(ProviderError::Api {
                provider: "test".into(),
                detail: "the model is down".into(),
            });
        }
        let reply = s.replies.lock().unwrap().get(who).cloned();
        Ok(text(reply.unwrap_or_else(|| format!("{who} here"))))
    }

    fn describe(&self) -> String {
        "group test model".into()
    }
}

async fn open(dir: &std::path::Path, script: Arc<Script>) -> Arc<Host> {
    let provider: Arc<dyn Provider> = Arc::new(Model(script));
    let ark = Ark::builder()
        .store_root(dir)
        .provider(provider)
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let app = App::new("Test").ark(ark).orchestrate(|ark, project, gate| {
        Ok(Orchestrator::builder(ark, project)
            .approvals(gate)
            .build()?)
    });
    ArkUi::new(app).open().await.unwrap().0
}

struct Setup {
    host: Arc<Host>,
    script: Arc<Script>,
    project: String,
    group: String,
}

fn member(project: &str, agent: &str) -> Value {
    json!({ "project": project, "agent": agent })
}

async fn setup(dir: &std::path::Path) -> Setup {
    let script = Arc::new(Script::default());
    let host = open(dir, script.clone()).await;
    let summary = host.call("summary", json!({})).await.unwrap();
    let project = summary["project"]["id"].as_str().unwrap().to_string();
    for (name, title, role) in [("scout", "Scout", "finds things"), ("ana", "Ana", "writes")] {
        host.call(
            "resource",
            json!({
                "ref": { "location": summary["location"], "kind": "project", "id": "" },
                "operation": { "kind": "spawn_agent", "input": {
                    "spec": { "name": name, "title": title, "role": role } } },
            }),
        )
        .await
        .unwrap();
    }
    let row = host
        .call(
            "group_create",
            json!({ "name": "Pair", "members": [member(&project, "scout"), member(&project, "ana")] }),
        )
        .await
        .unwrap();
    Setup {
        host,
        script,
        project,
        group: row["id"].as_str().unwrap().to_string(),
    }
}

impl Setup {
    async fn send(&self, text: &str) -> Value {
        self.host
            .call("group_send", json!({ "id": self.group, "text": text }))
            .await
            .unwrap()
    }

    async fn messages(&self) -> Value {
        self.host
            .call("group_messages", json!({ "id": self.group }))
            .await
            .unwrap()
    }

    /// The messages once there are `n`, and a moment later still `n`.
    async fn settle(&self, n: usize) -> Vec<Value> {
        for _ in 0..200 {
            let m = self.messages().await;
            let list = m["messages"].as_array().unwrap();
            if list.len() >= n && m["working"].as_array().unwrap().is_empty() {
                tokio::time::sleep(Duration::from_millis(300)).await;
                let after = self.messages().await;
                return after["messages"].as_array().unwrap().clone();
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("fewer than {n} messages: {}", self.messages().await);
    }

    fn agents_of(list: &[Value]) -> Vec<String> {
        list.iter()
            .map(|m| m["from"]["agent"].as_str().unwrap_or("person").to_string())
            .collect()
    }

    fn router_calls(&self) -> usize {
        self.script.router_calls.load(Ordering::SeqCst)
    }
}

#[tokio::test]
async fn an_at_mention_goes_to_that_agent_with_no_router_call() {
    let dir = tempfile::tempdir().unwrap();
    let t = setup(dir.path()).await;
    let sent = t.send("@ana what do you think?").await;
    assert_eq!(sent["by"], "mention");
    assert_eq!(sent["to"], json!([member(&t.project, "ana")]));
    assert_eq!(sent["message"]["text"], "@ana what do you think?");
    let list = t.settle(2).await;
    assert_eq!(Setup::agents_of(&list), ["person", "ana"]);
    assert_eq!(list[1]["text"], "Ana here");
    assert_eq!(t.router_calls(), 0);
}

#[tokio::test]
async fn the_router_names_two_members_and_both_answer() {
    let dir = tempfile::tempdir().unwrap();
    let t = setup(dir.path()).await;
    *t.script.router_answer.lock().unwrap() = "Ana, Scout".into();
    let sent = t.send("plan the launch").await;
    assert_eq!(sent["by"], "router");
    assert_eq!(sent["to"].as_array().unwrap().len(), 2);
    let list = t.settle(3).await;
    assert_eq!(Setup::agents_of(&list), ["person", "ana", "scout"]);
    assert_eq!(t.router_calls(), 1);
}

#[tokio::test]
async fn an_unusable_router_answer_falls_back_to_the_first_member() {
    let dir = tempfile::tempdir().unwrap();
    let t = setup(dir.path()).await;
    *t.script.router_answer.lock().unwrap() = "nobody in particular".into();
    let sent = t.send("hello?").await;
    assert_eq!(sent["by"], "fallback");
    assert_eq!(sent["to"], json!([member(&t.project, "scout")]));
    t.settle(2).await;
}

#[tokio::test]
async fn a_reply_that_names_another_member_hands_on() {
    let dir = tempfile::tempdir().unwrap();
    let t = setup(dir.path()).await;
    t.script
        .replies
        .lock()
        .unwrap()
        .insert("Scout".into(), "Found it. @Ana can you write it up?".into());
    t.send("@scout find the bug").await;
    let list = t.settle(3).await;
    assert_eq!(Setup::agents_of(&list), ["person", "scout", "ana"]);
    assert_eq!(t.router_calls(), 0);
}

#[tokio::test]
async fn a_ping_pong_stops_at_six_agent_replies() {
    let dir = tempfile::tempdir().unwrap();
    let t = setup(dir.path()).await;
    {
        let mut r = t.script.replies.lock().unwrap();
        r.insert("Scout".into(), "@Ana your turn".into());
        r.insert("Ana".into(), "@Scout your turn".into());
    }
    t.send("@scout start").await;
    let list = t.settle(7).await;
    assert_eq!(
        list.len(),
        7,
        "one person message and six replies: {list:?}"
    );
}

#[tokio::test]
async fn an_ended_member_is_skipped_and_left_out_of_the_rows() {
    let dir = tempfile::tempdir().unwrap();
    let t = setup(dir.path()).await;
    t.host
        .call("agent_end", json!({ "project": t.project, "agent": "ana" }))
        .await
        .unwrap();
    let rows = t.host.call("groups", json!({})).await.unwrap();
    assert_eq!(rows[0]["members"], json!([member(&t.project, "scout")]));
    let sent = t.send("@ana are you there?").await;
    assert_eq!(sent["by"], "single");
    assert_eq!(sent["to"], json!([member(&t.project, "scout")]));
    let list = t.settle(2).await;
    assert_eq!(Setup::agents_of(&list), ["person", "scout"]);
    assert_eq!(t.router_calls(), 0);
    assert!(!t.script.asked.lock().unwrap().contains(&"Ana".to_string()));
}

#[tokio::test]
async fn messages_keep_their_order_and_a_failed_task_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let t = setup(dir.path()).await;
    t.send("@scout first").await;
    t.settle(2).await;
    t.script.failing.lock().unwrap().push("Scout".into());
    t.send("@scout second").await;
    let list = t.settle(4).await;
    assert_eq!(
        Setup::agents_of(&list),
        ["person", "scout", "person", "scout"]
    );
    assert!(list[1]["failed"].is_null());
    assert_eq!(list[3]["failed"], true);
    assert!(list[3]["text"]
        .as_str()
        .unwrap()
        .contains("the model is down"));
    let ids: Vec<&str> = list.iter().map(|m| m["id"].as_str().unwrap()).collect();
    assert!(ids.windows(2).all(|w| w[0] < w[1]), "{ids:?}");
    let rows = t.host.call("groups", json!({})).await.unwrap();
    assert_eq!(rows[0]["last"]["from"], member(&t.project, "scout"));
}

#[tokio::test]
async fn a_second_turn_sees_only_what_came_since_its_last_reply() {
    let dir = tempfile::tempdir().unwrap();
    let t = setup(dir.path()).await;
    t.send("@scout alpha").await;
    t.settle(2).await;
    t.send("@ana beta").await;
    t.settle(4).await;
    t.send("@scout gamma").await;
    t.settle(6).await;
    let seen = t.script.seen.lock().unwrap().clone();
    assert_eq!(seen[2].0, "Scout");
    let third = &seen[2].1;
    assert!(third.contains("Person: @scout gamma") && third.contains("Ana: Ana here"));
    // Once from its thread's own history; a brief from the start would add a second.
    assert_eq!(third.matches("@scout alpha").count(), 1, "{third}");
    assert!(t
        .host
        .call("group_delete", json!({ "id": t.group }))
        .await
        .is_ok());
    assert!(t
        .host
        .call("group_messages", json!({ "id": t.group }))
        .await
        .is_err());
}

#[tokio::test]
async fn a_group_needs_two_live_agents() {
    let dir = tempfile::tempdir().unwrap();
    let t = setup(dir.path()).await;
    let one = json!({ "name": "Solo", "members": [member(&t.project, "scout")] });
    assert!(t.host.call("group_create", one).await.is_err());
    let ghost = json!({ "name": "G", "members": [member(&t.project, "scout"), member(&t.project, "ghost")] });
    assert!(t.host.call("group_create", ghost).await.is_err());
}
