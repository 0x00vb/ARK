//! An agent starts a server in its project and reads its page in its own browser (ADR 0046),
//! through the runtime as an app builds it: processes and the browser on, a model that asks.
//! Skipped without python3 or a Chromium-family browser.

#![cfg(unix)]

use std::sync::{Arc, Mutex};

use ark_sdk::prelude::ark_model::{Completion, MockProvider, ToolCall, Usage};
use ark_sdk::prelude::*;
use serde_json::json;

fn call(name: &str, args: serde_json::Value) -> Completion {
    Completion {
        content: String::new(),
        tool_calls: vec![ToolCall {
            id: format!("c-{name}"),
            name: name.into(),
            arguments: args,
        }],
        usage: Usage::default(),
    }
}

fn say(text: &str) -> Completion {
    Completion {
        content: text.into(),
        tool_calls: vec![],
        usage: Usage::default(),
    }
}

struct Allows;

#[async_trait::async_trait]
impl Approver for Allows {
    async fn decide(&self, _r: &ApprovalRequest) -> ApprovalDecision {
        ApprovalDecision::AllowOnce
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_reads_the_page_of_the_server_it_started() {
    let python = std::process::Command::new("python3").arg("-V").output();
    if !python.is_ok_and(|o| o.status.success()) {
        eprintln!("skipped: no python3");
        return;
    }
    if ark_sdk::prelude::ark_browser::cdp::find_browser().is_none() {
        eprintln!("skipped: no Chromium-family browser");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let other = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let other_port = other.local_addr().unwrap().port();
    let script = vec![
        call(
            "process.start",
            json!({ "command": format!("python3 -u -m http.server {port} --bind 127.0.0.1"), "port": port, "wait": 20 }),
        ),
        call(
            "browser.open",
            json!({ "url": format!("http://localhost:{port}/") }),
        ),
        call(
            "browser.open",
            json!({ "url": format!("http://localhost:{other_port}/") }),
        ),
        say("The server lists the project's folder."),
    ];
    let ark = Ark::builder()
        .store_root(dir.path())
        .provider(Arc::new(MockProvider::new("m", script)))
        .processes(true)
        .browser(true)
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let p = ark.find_or_create_project("p", "").unwrap();
    let mut session = ark.start_session(&p.id).unwrap();
    let results = Arc::new(Mutex::new(Vec::new()));
    let seen = results.clone();
    ark.ask_session(
        &mut session,
        "start the server and tell me what its page shows",
        TurnOptions {
            approvals: Some(Arc::new(ApprovalGate::new(Arc::new(Allows)))),
            ..Default::default()
        },
        move |e| {
            if let HarnessEvent::ToolResult {
                action,
                ok,
                summary,
                error,
                ..
            } = e
            {
                seen.lock()
                    .unwrap()
                    .push((action.clone(), ok, format!("{summary} {error:?}")));
            }
        },
    )
    .await
    .unwrap();
    let results = results.lock().unwrap().clone();
    let opened: Vec<_> = results
        .iter()
        .filter(|(a, ..)| a == "browser.open")
        .collect();
    assert_eq!(opened.len(), 2, "{results:#?}");
    assert!(opened[0].1, "its own server opens: {results:#?}");
    assert!(opened[0].2.contains("Directory listing"), "{results:#?}");
    assert!(
        !opened[1].1,
        "another program's port does not: {results:#?}"
    );
    assert!(opened[1].2.contains("refused"), "{results:#?}");
    drop(other);
}
