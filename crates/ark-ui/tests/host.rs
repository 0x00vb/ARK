//! The workspace host driven through its protocol with a mock model: approvals, cancellation,
//! steering, sessions and commands, the HTTP transport's access checks, and the token rule
//! (a GUI session sends the model what a CLI session sends).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ark_interact::{App, ChannelApprover, Start, TurnControl};
use ark_sdk::prelude::ark_model::{estimate_tokens, Completion, MockProvider, ToolCall, Usage};
use ark_sdk::prelude::*;
use ark_ui::{ArkUi, Host};
use serde_json::{json, Value};

struct Notes {
    saves: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl Adapter for Notes {
    fn name(&self) -> &str {
        "notes"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![
            ActionDescriptor::new("notes.read", "read the notes", RiskTier::Read),
            ActionDescriptor::new("notes.save", "save the notes", RiskTier::Mutate),
        ]
    }
    async fn run(
        &self,
        action: &str,
        _i: Value,
        _c: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        if action == "notes.save" {
            self.saves.fetch_add(1, Ordering::SeqCst);
            return Ok(ActionOutput::json(json!({ "saved": true })));
        }
        Ok(ActionOutput::json(
            json!({ "text": "the locket, the key, the door. ".repeat(200) }),
        ))
    }
}

/// A model that calls `action` once per turn, then answers at length, streaming slowly.
fn mock(action: &'static str) -> Arc<MockProvider> {
    mock_paced(action, Duration::from_millis(1))
}

fn mock_paced(action: &'static str, delay: Duration) -> Arc<MockProvider> {
    Arc::new(
        MockProvider::new("m", vec![])
            .with_chunk_delay(delay)
            .with_fallback(move |req| {
                if req.messages.iter().any(|m| m.tool_call_id.is_some()) {
                    Completion {
                        content: "Here is a long, careful answer about the notes. ".repeat(28),
                        tool_calls: vec![],
                        usage: Usage::default(),
                    }
                } else {
                    let call = ToolCall {
                        id: "c".into(),
                        name: action.into(),
                        arguments: json!({}),
                    };
                    Completion {
                        content: String::new(),
                        tool_calls: vec![call],
                        usage: Usage::default(),
                    }
                }
            }),
    )
}

fn app(dir: &std::path::Path, model: Arc<MockProvider>, saves: Arc<AtomicUsize>) -> App {
    let provider: Arc<dyn ark_model::Provider> = model;
    let ark = Ark::builder()
        .store_root(dir)
        .tool(Arc::new(Notes { saves }))
        .profile(
            AgentProfile::builder()
                .core_actions(["notes.read", "notes.save"])
                .build(),
        )
        .provider(provider)
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    App::new("Test").ark(ark)
}

async fn next(rx: &mut tokio::sync::broadcast::Receiver<Value>, kind: &str) -> Value {
    loop {
        let v = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .unwrap_or_else(|_| panic!("no `{kind}` event"))
            .unwrap();
        if v["kind"] == kind {
            return v;
        }
    }
}

async fn open(dir: &std::path::Path, action: &'static str, saves: Arc<AtomicUsize>) -> Arc<Host> {
    ArkUi::new(app(dir, mock(action), saves))
        .open()
        .await
        .unwrap()
        .0
}

#[tokio::test]
async fn approvals_travel_through_the_protocol() {
    let dir = tempfile::tempdir().unwrap();
    let saves = Arc::new(AtomicUsize::new(0));
    let host = open(dir.path(), "notes.save", saves.clone()).await;
    let mut rx = host.subscribe();
    host.call("send", json!({ "text": "save the notes" }))
        .await
        .unwrap();
    next(&mut rx, "turn_started").await;
    let ask = next(&mut rx, "approval").await;
    assert_eq!(ask["request"]["action"], "notes.save");
    // While the turn runs, commands that change the interaction are refused, reads are not.
    assert!(host
        .call("command", json!({ "line": "/new" }))
        .await
        .is_err());
    assert!(host.call("state", json!({})).await.is_ok());
    // Subscription usage is a read: a list, empty until a CLI reports.
    assert!(host
        .call("subscription_usage", json!({}))
        .await
        .unwrap()
        .is_array());
    let hello = host.call("hello", json!({})).await.unwrap();
    assert_eq!(
        hello["approvals"].as_array().unwrap().len(),
        1,
        "a reloaded window sees the question"
    );
    host.call(
        "approve",
        json!({ "id": ask["id"], "decision": "allow_once" }),
    )
    .await
    .unwrap();
    next(&mut rx, "approval_closed").await;
    let done = next(&mut rx, "turn_finished").await;
    assert_eq!(done["ok"], true);
    assert_eq!(saves.load(Ordering::SeqCst), 1);
    assert!(done["footer"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f == "1 tool"));

    // The stored turn carries its activity and its call detail.
    let turns = host.call("transcript", json!({})).await.unwrap();
    let acts = turns[0]["activity"].as_array().unwrap();
    assert!(acts
        .iter()
        .any(|a| a["kind"] == "approval" && a["label"] == "allowed once"));
    let call = host
        .call("call", json!({ "index": 0, "n": 1 }))
        .await
        .unwrap();
    assert_eq!(call["call"]["status"], "ok");
    assert_eq!(call["call"]["approval"]["decision"], "allow_once");
    assert!(call["invocation"]["output"]["saved"].as_bool().unwrap());
}

#[tokio::test]
async fn denial_cancel_and_late_steering() {
    let dir = tempfile::tempdir().unwrap();
    let saves = Arc::new(AtomicUsize::new(0));
    let host = open(dir.path(), "notes.save", saves.clone()).await;
    let mut rx = host.subscribe();

    host.call("send", json!({ "text": "save it" }))
        .await
        .unwrap();
    let ask = next(&mut rx, "approval").await;
    host.call("approve", json!({ "id": ask["id"], "decision": "deny" }))
        .await
        .unwrap();
    let done = next(&mut rx, "turn_finished").await;
    assert_eq!(done["ok"], true);
    assert_eq!(saves.load(Ordering::SeqCst), 0, "denied: nothing ran");

    // Stop while the question is open: the question closes and the turn ends cancelled.
    host.call("send", json!({ "text": "save it again" }))
        .await
        .unwrap();
    next(&mut rx, "approval").await;
    host.call("steer", json!({ "text": "never mind" }))
        .await
        .unwrap();
    host.call("cancel", json!({})).await.unwrap();
    next(&mut rx, "approval_closed").await;
    let done = next(&mut rx, "turn_finished").await;
    assert_eq!(done["cancelled"], true);
    assert_eq!(
        done["undelivered"],
        json!(["never mind"]),
        "the front-end sends it next"
    );
    assert!(!host.is_running());
}

#[tokio::test]
async fn sessions_commands_and_rename() {
    let dir = tempfile::tempdir().unwrap();
    let host = open(dir.path(), "notes.read", Arc::default()).await;
    let mut rx = host.subscribe();
    host.call("send", json!({ "text": "read the notes" }))
        .await
        .unwrap();
    next(&mut rx, "turn_finished").await;
    let first = host.call("summary", json!({})).await.unwrap()["session"]["id"].clone();

    let reply = host
        .call("command", json!({ "line": "/usage" }))
        .await
        .unwrap();
    assert_eq!(reply["report"]["data"]["view"], "usage");
    host.call("rename_session", json!({ "title": "Locket" }))
        .await
        .unwrap();
    host.call("new_session", json!({})).await.unwrap();
    let list = host.call("sessions", json!({})).await.unwrap();
    assert!(list
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["title"] == "Locket"));
    let back = host
        .call("open_session", json!({ "id": first }))
        .await
        .unwrap();
    assert_eq!(back["session"]["title"], "Locket");
    let turns = host.call("transcript", json!({})).await.unwrap();
    assert_eq!(
        turns.as_array().unwrap().len(),
        1,
        "the resumed session's transcript"
    );

    // A command whose effect is Submit starts a turn by itself.
    let ctx = host.call("context", json!({})).await.unwrap();
    assert!(ctx["included"].as_array().unwrap().len() == 9);
}

#[tokio::test]
async fn http_transport_checks_token_and_host() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let dir = tempfile::tempdir().unwrap();
    let (_host, served) = ArkUi::new(app(dir.path(), mock("notes.read"), Arc::default()))
        .serve_background("127.0.0.1:0")
        .await
        .unwrap();
    let raw = |req: String| async move {
        let mut s = tokio::net::TcpStream::connect(("127.0.0.1", served.port))
            .await
            .unwrap();
        s.write_all(req.as_bytes()).await.unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).await.unwrap();
        out
    };
    let host_h = format!("127.0.0.1:{}", served.port);
    let page = raw(format!("GET / HTTP/1.1\r\nHost: {host_h}\r\n\r\n")).await;
    assert!(page.starts_with("HTTP/1.1 200") && page.contains("<title>Test</title>"));
    let body = r#"{"method":"hello","params":{}}"#;
    let no_token = raw(format!(
        "POST /rpc HTTP/1.1\r\nHost: {host_h}\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    ))
    .await;
    assert!(no_token.starts_with("HTTP/1.1 401"));
    let wrong_host = raw(format!(
        "POST /rpc HTTP/1.1\r\nHost: evil.example:80\r\nx-ark-token: {}\r\nContent-Length: {}\r\n\r\n{body}",
        served.token,
        body.len()
    ))
    .await;
    assert!(wrong_host.starts_with("HTTP/1.1 421"));
    let ok = raw(format!(
        "POST /rpc HTTP/1.1\r\nHost: {host_h}\r\nx-ark-token: {}\r\nContent-Length: {}\r\n\r\n{body}",
        served.token,
        body.len()
    ))
    .await;
    assert!(
        ok.contains(r#""ok":true"#) && ok.contains(r#""name":"Test""#),
        "{ok}"
    );
}

/// `text` with every `inv:<hex>` id replaced by `inv:_`.
fn without_ids(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(i) = rest.find("inv:") {
        out.push_str(&rest[..i + 4]);
        rest = &rest[i + 4..];
        let hex = rest.chars().take_while(|c| c.is_ascii_hexdigit()).count();
        out.push('_');
        rest = &rest[hex..];
    }
    out.push_str(rest);
    out
}

fn request_tokens(r: &ark_model::mock::Recorded) -> u32 {
    estimate_tokens(&r.system)
        + r.messages
            .iter()
            .map(|m| estimate_tokens(&m.content) + 4)
            .sum::<u32>()
}

/// The regression the milestone is held to: 500 turns through the GUI host, with the page
/// loading transcripts, traces, context and sessions between turns, send the model the same
/// prompt sizes as 500 turns through the interaction directly (the CLI path).
#[tokio::test]
async fn gui_and_cli_send_the_same_context_for_500_turns() {
    const TURNS: usize = 500;
    let cli_dir = tempfile::tempdir().unwrap();
    let cli_model = mock_paced("notes.read", Duration::ZERO);
    let (approver, _rx) = ChannelApprover::channel();
    let mut ix = app(cli_dir.path(), cli_model.clone(), Arc::default())
        .open(&Start::default(), Arc::new(approver))
        .unwrap()
        .interaction;
    for t in 1..=TURNS {
        let q =
            format!("turn {t}: keep going with the revision and tell me what changed in detail");
        ix.run_turn(&q, &TurnControl::new(), |_, _| {})
            .await
            .result
            .unwrap();
    }

    let gui_dir = tempfile::tempdir().unwrap();
    let gui_model = mock_paced("notes.read", Duration::ZERO);
    let host = ArkUi::new(app(gui_dir.path(), gui_model.clone(), Arc::default()))
        .open()
        .await
        .unwrap()
        .0;
    let mut rx = host.subscribe();
    for t in 1..=TURNS {
        let q =
            format!("turn {t}: keep going with the revision and tell me what changed in detail");
        host.call("send", json!({ "text": q })).await.unwrap();
        next(&mut rx, "turn_finished").await;
        // What a window does between turns. None of it may reach the model.
        if t % 25 == 0 {
            host.call("transcript", json!({ "limit": 30 }))
                .await
                .unwrap();
            host.call("trace", json!({})).await.unwrap();
            host.call("call", json!({ "n": 1 })).await.unwrap();
            host.call("context", json!({})).await.unwrap();
            host.call("state", json!({})).await.unwrap();
            host.call("sessions", json!({})).await.unwrap();
            host.call("focus", json!({ "focus": null })).await.unwrap();
            host.call("command", json!({ "line": "/usage" }))
                .await
                .unwrap();
            host.call(
                "rename_session",
                json!({ "title": format!("renamed at {t}") }),
            )
            .await
            .unwrap();
        }
    }

    let cli: Vec<u32> = cli_model.recorded().iter().map(request_tokens).collect();
    let gui: Vec<u32> = gui_model.recorded().iter().map(request_tokens).collect();
    assert_eq!(cli.len(), gui.len(), "same number of model calls");
    let worst = cli
        .iter()
        .zip(&gui)
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .unwrap();
    assert_eq!(worst, 0, "every request is the same size");
    // Byte for byte: the same system prompt and messages. The tool lists differ only by
    // `present`, which a window can act on and a terminal cannot.
    for (c, g) in cli_model.recorded().iter().zip(gui_model.recorded().iter()) {
        assert_eq!(c.system, g.system);
        // Invocation ids are random per run; everything else must match.
        let text = |r: &ark_model::mock::Recorded| {
            r.messages
                .iter()
                .map(|m| without_ids(&m.content))
                .collect::<Vec<_>>()
        };
        assert_eq!(text(c), text(g));
        let names = |r: &ark_model::mock::Recorded| {
            r.tools
                .iter()
                .map(|t| t.name.clone())
                .filter(|n| n != "present")
                .collect::<Vec<_>>()
        };
        assert_eq!(names(c), names(g));
        assert!(g.tools.iter().any(|t| t.name == "present"));
        assert!(c.tools.iter().all(|t| t.name != "present"));
    }
    let (at_50, at_500) = (gui[2 * 49 + 1], gui[2 * 499 + 1]);
    println!("\n| path | requests | turn 50 answer request | turn 500 answer request |");
    println!("|---|---:|---:|---:|");
    println!(
        "| CLI (Interaction) | {} | {} | {} |",
        cli.len(),
        cli[2 * 49 + 1],
        cli[2 * 499 + 1]
    );
    println!("| GUI (Host) | {} | {at_50} | {at_500} |", gui.len());
    assert!(
        at_500 <= at_50 + at_50 / 50,
        "the prompt plateaus: {at_50} vs {at_500}"
    );
}

#[tokio::test]
async fn events_are_numbered_and_a_late_window_gets_the_running_turn() {
    let dir = tempfile::tempdir().unwrap();
    let saves = Arc::new(AtomicUsize::new(0));
    let model = mock_paced("notes.read", Duration::from_millis(5));
    let (host, _) = ArkUi::new(app(dir.path(), model, saves))
        .open()
        .await
        .unwrap();
    let mut rx = host.subscribe();
    host.call("send", json!({ "text": "read the notes" }))
        .await
        .unwrap();
    // Wait until some answer text has streamed, then join as a new window would.
    let mut seen: Vec<Value> = Vec::new();
    loop {
        let v = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .unwrap()
            .unwrap();
        let text = v["kind"] == "text";
        seen.push(v);
        if text && seen.iter().filter(|e| e["kind"] == "text").count() > 5 {
            break;
        }
    }
    let hello = host.call("hello", json!({})).await.unwrap();
    let live = hello["live"].as_array().unwrap();
    let seq = hello["seq"].as_u64().unwrap();
    assert_eq!(
        live[0]["kind"], "turn_started",
        "the replay starts at the turn"
    );
    assert_eq!(live.last().unwrap()["seq"].as_u64().unwrap(), seq);
    assert!(
        live.len() >= seen.len(),
        "everything this window saw is in the replay"
    );
    // The rest of the turn arrives after `seq`, numbered one by one.
    let mut last = seen.last().unwrap()["seq"].as_u64().unwrap();
    loop {
        let v = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .unwrap()
            .unwrap();
        let n = v["seq"].as_u64().unwrap();
        assert_eq!(n, last + 1, "no gaps, no repeats");
        last = n;
        if v["kind"] == "turn_finished" {
            break;
        }
    }
    let after = host.call("hello", json!({})).await.unwrap();
    assert!(
        after["live"].as_array().unwrap().is_empty(),
        "nothing to replay between turns"
    );
    assert_eq!(after["seq"].as_u64().unwrap(), last);
}
