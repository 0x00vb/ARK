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

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn project_terminal_opens_without_changing_the_chat_and_deduplicates_writes() {
    let store = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let host = ArkUi::new(app(store.path(), mock("notes.read"), Arc::default()))
        .open()
        .await
        .unwrap()
        .0;
    let summary = host
        .call(
            "add_project",
            json!({ "name": "Shell only", "path": work.path() }),
        )
        .await
        .unwrap();
    let session = summary["session"]["id"].as_str().unwrap().to_string();
    let project = summary["project"]["id"].as_str().unwrap().to_string();

    let opened = host
        .call(
            "project_terminal_open",
            json!({ "project": project, "place": "", "here": "This computer", "cols": 80, "rows": 24 }),
        )
        .await
        .unwrap();
    assert_eq!(opened["ref"]["authority"]["scope"], "project");
    assert_eq!(opened["ref"]["location"]["session"], "");
    assert_eq!(
        host.call("summary", json!({})).await.unwrap()["session"]["id"],
        session,
        "opening a project Terminal must not select or create a chat"
    );
    let second = host
        .call(
            "project_terminal_open",
            json!({ "project": project, "place": "", "here": "This computer", "cols": 80, "rows": 24 }),
        )
        .await
        .unwrap();
    assert_eq!(second["ref"]["id"], opened["ref"]["id"]);
    assert_ne!(
        second["ref"]["authority"]["route"], opened["ref"]["authority"]["route"],
        "each window gets its own opaque route"
    );

    for forged in [
        {
            let mut r = opened["ref"].clone();
            r["location"]["project"] = json!("another-project");
            r
        },
        {
            let mut r = opened["ref"].clone();
            r["location"]["runtime"] = json!("another-runtime");
            r
        },
        {
            let mut r = opened["ref"].clone();
            r["id"] = json!("term-forged");
            r
        },
        {
            let mut r = opened["ref"].clone();
            r["authority"]["route"] = json!("route-forged");
            r
        },
    ] {
        assert!(host
            .call(
                "resource",
                json!({ "ref": forged, "operation": { "kind": "terminal_read", "input": { "generation": opened["generation"], "after": 0 } } }),
            )
            .await
            .is_err());
    }

    let write = |sequence| {
        json!({
            "ref": opened["ref"],
            "operation": { "kind": "terminal_write", "input": { "data": "printf x\\n", "sequence": sequence } },
        })
    };
    host.call("resource", write(1)).await.unwrap();
    let duplicate = host
        .call("resource", write(1))
        .await
        .unwrap_err()
        .to_string();
    assert!(
        duplicate.contains("duplicate or out of order"),
        "{duplicate}"
    );
    host.call(
        "resource",
        json!({ "ref": opened["ref"], "operation": { "kind": "terminal_close", "input": {} } }),
    )
    .await
    .unwrap();
    assert!(
        host.call("resource", write(2)).await.is_err(),
        "an ended route is stale"
    );
    assert!(
        host.call(
            "resource",
            json!({ "ref": second["ref"], "operation": { "kind": "terminal_read", "input": { "generation": second["generation"], "after": 0 } } }),
        )
        .await
        .is_err(),
        "ending one alias invalidates every route to the shared shell"
    );

    let natural = host
        .call(
            "project_terminal_open",
            json!({ "project": project, "place": "", "here": "This computer", "cols": 80, "rows": 24 }),
        )
        .await
        .unwrap();
    assert!(host
        .call(
            "resource",
            json!({ "ref": natural["ref"], "operation": { "kind": "terminal_read", "input": { "generation": "stale-generation", "after": 0 } } }),
        )
        .await
        .is_err());
    host.call(
        "resource",
        json!({ "ref": natural["ref"], "operation": { "kind": "terminal_write", "input": { "data": "exit\n", "sequence": 1 } } }),
    )
    .await
    .unwrap();
    let mut stale = false;
    // A heavily loaded CI host can take more than a second to schedule the PTY reader after the
    // shell exits. The route must still disappear as soon as that exit is observed.
    for _ in 0..500 {
        if host
            .call(
                "resource",
                json!({ "ref": natural["ref"], "operation": { "kind": "terminal_read", "input": { "generation": natural["generation"], "after": 0 } } }),
            )
            .await
            .is_err()
        {
            stale = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(stale, "a natural shell exit invalidates its project route");
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

async fn project_call(host: &Arc<Host>, operation: &str, input: Value) -> anyhow::Result<Value> {
    let location = host.call("summary", json!({})).await?["location"].clone();
    host.call(
        "resource",
        json!({
            "ref": { "location": location, "kind": "project", "id": "" },
            "operation": { "kind": operation, "input": input },
        }),
    )
    .await
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
    assert!(project_call(&host, "state", json!({})).await.is_ok());
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
    let included = ctx["included"].as_array().unwrap();
    assert_eq!(included.len(), 10);
    let memory = included
        .iter()
        .find(|section| section["key"] == "memory")
        .unwrap();
    assert_eq!(memory["label"], "Project memory");
    assert_eq!(memory["tokens"], 0);
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
            project_call(&host, "state", json!({})).await.unwrap();
            host.call("sessions", json!({})).await.unwrap();
            project_call(&host, "focus", json!({ "focus": null }))
                .await
                .unwrap();
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
    // Every event of the turn up to `seq` is in the replay.
    let turn_seen = seen.iter().rfind(|e| !e["session"].is_null()).unwrap();
    assert!(live.last().unwrap()["seq"].as_u64().unwrap() >= turn_seen["seq"].as_u64().unwrap());
    assert!(live.last().unwrap()["seq"].as_u64().unwrap() <= seq);
    assert!(
        live.len() >= seen.iter().filter(|e| !e["session"].is_null()).count(),
        "everything of the turn this window saw is in the replay"
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
    // Then the list of working chats changes.
    let working = next(&mut rx, "working").await;
    assert_eq!(working["seq"].as_u64().unwrap(), last + 1);
    last += 1;
    let after = host.call("hello", json!({})).await.unwrap();
    assert!(
        after["live"].as_array().unwrap().is_empty(),
        "nothing to replay between turns"
    );
    assert_eq!(after["seq"].as_u64().unwrap(), last);
}

/// Moving to another chat while one works leaves it working in the background: its question
/// waits, the window starts and finishes a turn in the new chat, and going back finds the first
/// one still running. Events and questions say which chat they belong to.
#[tokio::test]
async fn a_chat_keeps_working_while_the_window_moves_to_another() {
    let dir = tempfile::tempdir().unwrap();
    let saves = Arc::new(AtomicUsize::new(0));
    let host = open(dir.path(), "notes.save", saves.clone()).await;
    let mut rx = host.subscribe();
    host.call("send", json!({ "text": "save the notes" }))
        .await
        .unwrap();
    let started = next(&mut rx, "turn_started").await;
    let first = started["session"].as_str().unwrap().to_string();
    let ask = next(&mut rx, "approval").await;
    assert_eq!(ask["session"], first.as_str());

    // A new chat while the first one waits for an answer.
    let moved = host.call("new_session", json!({})).await.unwrap();
    let second = moved["session"]["id"].as_str().unwrap().to_string();
    assert_ne!(first, second);
    // Two chats, no empty one left behind by the move.
    let chats = host.call("sessions", json!({})).await.unwrap();
    assert_eq!(chats.as_array().unwrap().len(), 2, "{chats}");
    assert_eq!(
        chats
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["working"] == true)
            .count(),
        1
    );
    assert_eq!(moved["running"], false);
    let working = host.call("working", json!({})).await.unwrap();
    assert_eq!(working[0]["session"], first.as_str());
    assert_eq!(working[0]["input"], "save the notes");
    let hello = host.call("hello", json!({})).await.unwrap();
    assert!(hello["running"].is_null() && hello["live"].as_array().unwrap().is_empty());
    assert_eq!(
        hello["approvals"][0]["session"],
        first.as_str(),
        "its question waits, marked"
    );
    // Lists answer while a turn runs.
    assert!(host.call("projects", json!({})).await.is_ok());

    // Back to the first: it is still running, and a window rebuilds its turn.
    let back = host
        .call("open_session", json!({ "id": first }))
        .await
        .unwrap();
    assert_eq!(back["session"]["id"], first.as_str());
    assert_eq!(back["running"], true);
    let hello = host.call("hello", json!({})).await.unwrap();
    assert_eq!(hello["running"], "save the notes");
    assert!(!hello["live"].as_array().unwrap().is_empty());
    // And away again, to the second chat, which runs a turn of its own meanwhile.
    host.call("open_session", json!({ "id": second }))
        .await
        .unwrap();
    host.call("send", json!({ "text": "save the other notes" }))
        .await
        .unwrap();
    let other = next(&mut rx, "approval").await;
    assert_eq!(other["session"], second.as_str());
    host.call(
        "approve",
        json!({ "id": other["id"], "decision": "allow_once" }),
    )
    .await
    .unwrap();
    let done = next(&mut rx, "turn_finished").await;
    assert_eq!(done["session"], second.as_str());
    assert!(done["summary"].is_object(), "the chat on screen");

    // The first chat's answer finishes it in the background.
    host.call(
        "approve",
        json!({ "id": ask["id"], "decision": "allow_once" }),
    )
    .await
    .unwrap();
    let done = next(&mut rx, "turn_finished").await;
    assert_eq!(done["session"], first.as_str());
    assert!(done["summary"].is_null(), "not the chat on screen");
    assert_eq!(saves.load(Ordering::SeqCst), 2);
    assert!(host
        .call("working", json!({}))
        .await
        .unwrap()
        .as_array()
        .unwrap()
        .is_empty());
    let turns = host
        .call("transcript", json!({ "session": first }))
        .await
        .unwrap();
    assert_eq!(turns.as_array().unwrap().len(), 1);
    host.call("open_session", json!({ "id": first }))
        .await
        .unwrap();
    assert_eq!(
        host.call("transcript", json!({}))
            .await
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

/// A model that refuses every request (a permanent error: not retried).
struct Refuses;

#[async_trait::async_trait]
impl ark_model::Provider for Refuses {
    async fn complete(
        &self,
        _system: &str,
        _messages: &[ark_model::Message],
        _tools: &[ark_model::ToolDef],
    ) -> ark_model::Result<Completion> {
        Err(ark_model::ProviderError::Api {
            provider: "test".into(),
            detail: "the model refused".into(),
        })
    }
    fn describe(&self) -> String {
        "refuses".into()
    }
}

/// ADR 0048: a turn that fails says so and offers to continue at once, in the live window and
/// in the stored chat; the next turn takes it away.
#[tokio::test]
async fn a_failed_turn_offers_to_continue_live_and_after() {
    let dir = tempfile::tempdir().unwrap();
    let ark = Ark::builder()
        .store_root(dir.path())
        .provider(Arc::new(Refuses) as Arc<dyn ark_model::Provider>)
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let (host, _) = ArkUi::new(App::new("Test").ark(ark)).open().await.unwrap();
    let mut rx = host.subscribe();
    host.call("send", json!({ "text": "hello" })).await.unwrap();
    let done = next(&mut rx, "turn_finished").await;
    assert!(
        done["error"].as_str().unwrap().contains("refused"),
        "{done}"
    );
    assert_eq!(done["resume"], true, "{done}");
    let session = host.call("summary", json!({})).await.unwrap()["session"]["id"].clone();
    let turns = host
        .call("transcript", json!({ "session": session }))
        .await
        .unwrap();
    assert_eq!(turns[0]["resume"], true, "{turns}");
    assert!(turns[0]["unfinished"]
        .as_str()
        .unwrap()
        .starts_with("Did not finish: "));
}

/// A model that reads one page of the notes per call and answers only at every `answer_at`-th
/// call it gets. Each path (CLI, GUI) gets its own, so both see the same sequence.
fn mock_chain(answer_at: usize) -> Arc<MockProvider> {
    let calls = AtomicUsize::new(0);
    Arc::new(MockProvider::new("m", vec![]).with_fallback(move |_| {
        let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
        if n.is_multiple_of(answer_at) {
            return Completion {
                content: "The notes are about a locket, a key and a door.".into(),
                tool_calls: vec![],
                usage: Usage::default(),
            };
        }
        Completion {
            content: String::new(),
            tool_calls: vec![ToolCall {
                id: format!("c{n}"),
                name: "notes.read".into(),
                arguments: json!({ "page": n }),
            }],
            usage: Usage::default(),
        }
    }))
}

fn app_with_steps(dir: &std::path::Path, model: Arc<MockProvider>, max_steps: usize) -> App {
    let provider: Arc<dyn ark_model::Provider> = model;
    let ark = Ark::builder()
        .store_root(dir)
        .tool(Arc::new(Notes {
            saves: Arc::default(),
        }))
        .profile(
            AgentProfile::builder()
                .core_actions(["notes.read", "notes.save"])
                .build(),
        )
        .provider(provider)
        .planner(false)
        .critic(false)
        .max_steps(max_steps)
        // Small, so a long turn drops rounds behind the ledger.
        .context_budget(700)
        .build()
        .unwrap();
    App::new("Test").ark(ark)
}

/// ADR 0050: a task that runs past the step limit is continued by "continue" (a chain of
/// turns), and its long turns drop rounds behind a ledger. The GUI host sends the model the
/// same requests as the CLI path, byte for byte, through the chain and the dropped rounds.
#[tokio::test]
async fn gui_and_cli_send_the_same_context_with_a_chain_and_dropped_rounds() {
    const STEPS: usize = 12;
    const ANSWER_AT: usize = 30;
    let says = [
        "read every page of the notes and tell me what they are about",
        "continue",
        "continue",
        "now read them again and tell me if anything changed",
        "continue",
        "continue",
    ];

    let cli_dir = tempfile::tempdir().unwrap();
    let cli_model = mock_chain(ANSWER_AT);
    let (approver, _rx) = ChannelApprover::channel();
    let mut ix = app_with_steps(cli_dir.path(), cli_model.clone(), STEPS)
        .open(&Start::default(), Arc::new(approver))
        .unwrap()
        .interaction;
    for q in says {
        // A turn stopped by the step limit is an outcome here, not a failure of the test.
        let _ = ix.run_turn(q, &TurnControl::new(), |_, _| {}).await;
    }

    let gui_dir = tempfile::tempdir().unwrap();
    let gui_model = mock_chain(ANSWER_AT);
    let host = ArkUi::new(app_with_steps(gui_dir.path(), gui_model.clone(), STEPS))
        .open()
        .await
        .unwrap()
        .0;
    let mut rx = host.subscribe();
    for q in says {
        host.call("send", json!({ "text": q })).await.unwrap();
        next(&mut rx, "turn_finished").await;
        // What a window does between turns. None of it may reach the model.
        host.call("transcript", json!({})).await.unwrap();
        host.call("context", json!({})).await.unwrap();
        host.call("trace", json!({})).await.unwrap();
    }

    let (cli, gui) = (cli_model.recorded(), gui_model.recorded());
    assert_eq!(cli.len(), gui.len(), "same number of model calls");
    // A turn's line in the conversation lists all its invocation ids: random per run too.
    let ids = |t: &str| {
        let mut out = String::new();
        let mut rest = t;
        while let Some(i) = rest.find("[inv:_,") {
            out.push_str(&rest[..i + 6]);
            rest = &rest[i + 6..];
            rest = &rest[rest.find(']').unwrap_or(0)..];
        }
        out.push_str(rest);
        out
    };
    let text = |r: &ark_model::mock::Recorded| {
        r.messages
            .iter()
            .map(|m| ids(&without_ids(&m.content)))
            .collect::<Vec<_>>()
    };
    for (i, (c, g)) in cli.iter().zip(gui.iter()).enumerate() {
        assert_eq!(c.system, g.system, "system prompt of call {i}");
        assert_eq!(text(c), text(g), "messages of call {i}");
    }
    let all = |needle: &str| {
        gui.iter()
            .filter(|r| r.messages.iter().any(|m| m.content.contains(needle)))
            .count()
    };
    assert!(
        all("[Earlier steps of this task") > 0,
        "rounds were dropped behind the ledger"
    );
    assert!(
        all("Your last turn stopped before it finished") > 0,
        "a continued turn carries the stopped one over"
    );
}

/// ADR 0052: removing this computer from another forgets there only the keys it sent. A key
/// saved there since (here put straight into the store, without the provider check) stays.
#[tokio::test]
async fn forgetting_sent_keys_removes_only_the_key_that_was_sent() {
    use sha2::Digest;
    let dir = tempfile::tempdir().unwrap();
    let secrets = Arc::new(ark_connect::MemorySecrets::default());
    let conns = ark_connect::Connections::open(
        dir.path(),
        ark_connect::Catalog::builtin(),
        secrets.clone(),
    );
    let keys = conns.api_keys().clone();
    let put = |provider: &str, key: &str| {
        ark_connect::Secrets::set(secrets.as_ref(), &format!("api-key:{provider}"), key).unwrap();
    };
    put("anthropic", "sk-ant-sent");
    put("openai", "sk-openai-saved-there");
    std::fs::write(
        dir.path().join("api-keys.json"),
        r#"["anthropic","openai"]"#,
    )
    .unwrap();
    let ark = Ark::builder()
        .store_root(dir.path())
        .provider(Arc::new(MockProvider::new("m", vec![])) as Arc<dyn ark_model::Provider>)
        .connections_with(conns)
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let host = ArkUi::new(App::new("Test").ark(ark))
        .open()
        .await
        .unwrap()
        .0;
    let hash = |k: &str| format!("{:x}", sha2::Sha256::digest(k.as_bytes()));

    let out = host
        .call(
            "chat_key",
            json!({ "forget": { "anthropic": hash("sk-ant-sent"), "openai": hash("sk-openai-sent") } }),
        )
        .await
        .unwrap();

    assert_eq!(out["saved"], json!(["openai"]), "{out}");
    assert_eq!(keys.saved_key("anthropic"), None);
    assert_eq!(
        keys.saved_key("openai").as_deref(),
        Some("sk-openai-saved-there")
    );
}

/// A Browser tab the person had open in one project is not what they look at in the next: after
/// a project switch the agent is not told about the other project's page, neither from the
/// turn before nor from a report that arrived just before the switch.
#[tokio::test]
async fn another_projects_browser_tab_does_not_follow_a_project_switch() {
    let dir = tempfile::tempdir().unwrap();
    let model = Arc::new(
        MockProvider::new("m", vec![]).with_fallback(|_| Completion {
            content: "Done.".into(),
            tool_calls: vec![],
            usage: Usage::default(),
        }),
    );
    let host = ArkUi::new(app(dir.path(), model.clone(), Arc::default()))
        .open()
        .await
        .unwrap()
        .0;
    let shop = json!({ "focus": { "surface": {
        "kind_tag": "browser", "title": "localhost", "locator": "http://localhost:3000/shop"
    } } });
    let sees_shop = || {
        let last = model.recorded().last().cloned().unwrap();
        format!("{} {:?}", last.system, last.messages).contains("localhost:3000/shop")
    };
    let mut rx = host.subscribe();
    project_call(&host, "focus", shop.clone()).await.unwrap();
    host.call("send", json!({ "text": "is the shop running?" }))
        .await
        .unwrap();
    next(&mut rx, "turn_finished").await;
    assert!(sees_shop(), "the project's own turn is told about its tab");

    project_call(&host, "focus", shop).await.unwrap();
    host.call("add_project", json!({ "name": "bun" }))
        .await
        .unwrap();
    host.call("send", json!({ "text": "is the app running?" }))
        .await
        .unwrap();
    next(&mut rx, "turn_finished").await;
    assert!(
        !sees_shop(),
        "the other project's tab reached this project's turn"
    );
}
