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
            ActionDescriptor::new("notes.boom", "a tool that panics", RiskTier::Read),
        ]
    }
    async fn run(
        &self,
        action: &str,
        _i: Value,
        _c: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        if action == "notes.boom" {
            panic!("byte range starts at 1 but ends at 0");
        }
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
                .core_actions(["notes.read", "notes.save", "notes.boom"])
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
async fn a_tool_that_panics_fails_its_call_and_the_turn_goes_on() {
    let dir = tempfile::tempdir().unwrap();
    let host = open(dir.path(), "notes.boom", Arc::new(AtomicUsize::new(0))).await;
    let mut rx = host.subscribe();
    host.call("send", json!({ "text": "open it" }))
        .await
        .unwrap();
    let done = next(&mut rx, "turn_finished").await;
    assert_eq!(done["ok"], true, "{done}");
    assert!(!host.is_running(), "the chat is free again");
    host.call("send", json!({ "text": "again" })).await.unwrap();
    next(&mut rx, "turn_finished").await;
}

struct Panics;

#[async_trait::async_trait]
impl ark_model::Provider for Panics {
    async fn complete(
        &self,
        _system: &str,
        _messages: &[ark_model::Message],
        _tools: &[ark_model::ToolDef],
    ) -> ark_model::Result<Completion> {
        panic!("the model client broke");
    }
    fn describe(&self) -> String {
        "panics".into()
    }
}

#[tokio::test]
async fn a_turn_that_crashes_ends_with_an_error_and_frees_the_chat() {
    let dir = tempfile::tempdir().unwrap();
    let ark = Ark::builder()
        .store_root(dir.path())
        .provider(Arc::new(Panics) as Arc<dyn ark_model::Provider>)
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let (host, _) = ArkUi::new(App::new("Test").ark(ark)).open().await.unwrap();
    let mut rx = host.subscribe();
    host.call("send", json!({ "text": "hello" })).await.unwrap();
    let done = next(&mut rx, "turn_finished").await;
    assert_eq!(done["ok"], false, "{done}");
    assert!(
        done["error"]
            .as_str()
            .unwrap()
            .contains("model client broke"),
        "{done}"
    );
    assert!(!host.is_running(), "the chat is free again");
    host.call("send", json!({ "text": "again" })).await.unwrap();
    next(&mut rx, "turn_finished").await;
}

/// A model that reads images and answers at once, so a test sees what it was sent.
struct Sees(Arc<MockProvider>);

#[async_trait::async_trait]
impl ark_model::Provider for Sees {
    async fn complete(
        &self,
        system: &str,
        messages: &[ark_model::Message],
        tools: &[ark_model::ToolDef],
    ) -> ark_model::Result<Completion> {
        self.0.complete(system, messages, tools).await
    }
    fn describe(&self) -> String {
        "sees".into()
    }
    fn reads_pictures(&self) -> bool {
        true
    }
}

/// A 1x1 PNG.
const PIXEL: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";

/// A host over a model that answers at once; `reads` says whether it reads images.
async fn seeing(dir: &std::path::Path, reads: bool) -> (Arc<Host>, Arc<MockProvider>) {
    let model = Arc::new(
        MockProvider::new("m", vec![]).with_fallback(|_| Completion {
            content: "It is one pixel.".into(),
            tool_calls: vec![],
            usage: Usage::default(),
        }),
    );
    let provider: Arc<dyn ark_model::Provider> = if reads {
        Arc::new(Sees(model.clone()))
    } else {
        model.clone()
    };
    let ark = Ark::builder()
        .store_root(dir)
        .provider(provider)
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let (host, _) = ArkUi::new(App::new("Test").ark(ark)).open().await.unwrap();
    (host, model)
}

/// What the page does for a dropped file: send it in parts, then add it to the project.
async fn drop_file(host: &Arc<Host>, name: &str, data: &str) -> Value {
    let location = host.call("summary", json!({})).await.unwrap()["location"].clone();
    let call = |operation: &'static str, input: Value| {
        host.call(
            "resource",
            json!({
                "ref": { "location": location, "kind": "file", "id": "" },
                "operation": { "kind": operation, "input": input },
            }),
        )
    };
    let upload = format!("drop-{name}");
    call(
        "upload_parts",
        json!({ "upload": upload, "parts": [{ "path": name, "offset": 0, "data": data }] }),
    )
    .await
    .unwrap();
    call("upload_add", json!({ "upload": upload }))
        .await
        .unwrap()
}

#[tokio::test]
async fn an_attached_image_reaches_a_model_that_reads_images_and_stays_with_its_turn() {
    let dir = tempfile::tempdir().unwrap();
    let (host, model) = seeing(dir.path(), true).await;
    let mut rx = host.subscribe();
    let added = drop_file(&host, "shot.png", PIXEL).await;
    assert_eq!(added["as"]["shot.png"], "shot.png");
    host.call(
        "send",
        json!({ "text": "what is this", "images": ["shot.png"] }),
    )
    .await
    .unwrap();
    let done = next(&mut rx, "turn_finished").await;
    assert_eq!(done["ok"], true, "{done}");
    let sent: Vec<_> = model
        .recorded()
        .into_iter()
        .flat_map(|r| r.messages)
        .flat_map(|m| m.pictures)
        .collect();
    assert!(!sent.is_empty(), "the model was shown the image");
    assert!(sent.iter().all(|p| p.media_type == "image/png"));
    let turns = host.call("transcript", json!({})).await.unwrap();
    assert_eq!(turns[0]["images"], json!(["shot.png"]));
    assert_eq!(turns[0]["input"], "what is this");
    // The next message carries none, and its stored turn lists none.
    host.call("send", json!({ "text": "and now" }))
        .await
        .unwrap();
    next(&mut rx, "turn_finished").await;
    let turns = host.call("transcript", json!({})).await.unwrap();
    assert!(turns[1].get("images").is_none(), "{}", turns[1]);
}

#[tokio::test]
async fn a_model_that_reads_no_images_is_told_so_and_the_turn_still_lists_the_image() {
    let dir = tempfile::tempdir().unwrap();
    let (host, model) = seeing(dir.path(), false).await;
    let mut rx = host.subscribe();
    drop_file(&host, "shot.png", PIXEL).await;
    host.call(
        "send",
        json!({ "text": "what is this", "images": ["shot.png"] }),
    )
    .await
    .unwrap();
    next(&mut rx, "turn_finished").await;
    let calls = model.recorded();
    assert!(calls
        .iter()
        .flat_map(|r| &r.messages)
        .all(|m| m.pictures.is_empty()));
    assert!(calls
        .iter()
        .flat_map(|r| &r.messages)
        .any(|m| m.content.contains("this model is not sent images")));
    let turns = host.call("transcript", json!({})).await.unwrap();
    assert_eq!(turns[0]["images"], json!(["shot.png"]));
}

#[tokio::test]
async fn images_outside_the_project_or_not_images_are_refused_and_nothing_starts() {
    let dir = tempfile::tempdir().unwrap();
    let (host, _) = seeing(dir.path(), true).await;
    drop_file(&host, "notes.txt", "aGVsbG8=").await;
    for images in [
        json!(["../shot.png"]),
        json!(["notes.txt"]),
        json!(["missing.png"]),
    ] {
        let error = host
            .call("send", json!({ "text": "look", "images": images }))
            .await
            .unwrap_err();
        assert!(!error.to_string().is_empty());
        assert!(!host.is_running(), "a refused message starts no turn");
    }
    let many: Vec<String> = (0..9).map(|i| format!("{i}.png")).collect();
    let error = host
        .call("send", json!({ "text": "look", "images": many }))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("at most"), "{error}");
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

#[tokio::test]
async fn addressed_chat_reads_include_durable_delegated_participants() {
    let root = tempfile::tempdir().unwrap();
    let host = open(root.path(), "notes.read", Arc::default()).await;
    let summary = host.call("summary", json!({})).await.unwrap();
    let project = summary["project"]["id"].as_str().unwrap();
    let session = summary["session"]["id"].as_str().unwrap();
    let store = ark_store::Store::open(root.path()).unwrap();
    store
        .put_record(
            "orch_task",
            "participant-root",
            project,
            &json!({
                "id":"participant-root", "project":project, "owner":"Lead", "report_to":session
            }),
        )
        .unwrap();
    store.put_record("orch_task", "participant-child", project, &json!({
        "id":"participant-child", "project":project, "owner":"Writer", "parent":"participant-root"
    })).unwrap();
    let read = host
        .call("chat_read", json!({"session":session}))
        .await
        .unwrap();
    assert_eq!(
        read["summary"]["session"]["participants"],
        json!(["Lead", "Writer"])
    );
    store
        .put_record(
            "orch_task",
            "participant-broken",
            project,
            &json!({"id":"participant-broken"}),
        )
        .unwrap();
    let read = host
        .call("chat_read", json!({"session":session}))
        .await
        .unwrap();
    assert!(read["summary"]["session"]["participants"].is_null());
}

#[tokio::test(flavor = "multi_thread")]
async fn work_receipts_keep_models_attachments_and_retry_identity_through_the_host() {
    use ark_interact::ark_orchestrate::{Models, Orchestrator};
    let dir = tempfile::tempdir().unwrap();
    let reply = |text: &str| Completion {
        content: text.into(),
        tool_calls: vec![],
        usage: Usage::default(),
    };
    let a = Arc::new(MockProvider::new("a", vec![reply("answer A")]));
    let b = Arc::new(MockProvider::new("b", vec![reply("answer B")]));
    let ark = Ark::builder()
        .store_root(dir.path())
        .provider(a.clone())
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.create_project("work", "test").unwrap();
    let engine = ark.shared_engine();
    let artifact_id = engine.store.put_blob(b"attachment contents").unwrap();
    let artifact = Artifact::new(
        artifact_id.clone(),
        ArtifactKind::Text,
        "brief.txt",
        19,
        Provenance::Ingested {
            path: "brief.txt".into(),
        },
    );
    engine.store.put_artifact(&project.id, &artifact).unwrap();
    let o = Orchestrator::builder(&ark, &project)
        .models(Arc::new(
            Models::builder()
                .default_model("a")
                .provider("a", a.clone())
                .provider("b", b.clone())
                .build(),
        ))
        .build()
        .unwrap();
    let chosen = project.clone();
    let app = App::new("work test")
        .ark(ark)
        .project(move |_| Ok(chosen.clone()))
        .orchestrator(o.clone());
    let host = ArkUi::new(app).open().await.unwrap().0;
    let input = json!({"request_id":"one", "target":{"kind":"general","model":"a"},
        "task":{"objective":"read the attachment", "artifacts":[artifact_id]}});
    let receipt = project_call(&host, "work_admit", input.clone())
        .await
        .unwrap();
    assert!(o.agents().is_empty());
    assert!(a.calls.lock().unwrap().is_empty());
    assert_eq!(
        project_call(&host, "work_admit", input).await.unwrap(),
        receipt
    );
    assert_eq!(
        project_call(&host, "work_receipt", json!({"request_id":"one"}))
            .await
            .unwrap(),
        receipt
    );
    assert!(host
        .call(
            "chat_send",
            json!({"session":receipt["conversation"],"text":"different message"})
        )
        .await
        .is_err());
    let mut rx = host.subscribe();
    project_call(&host, "work_execute", json!({"receipt":receipt["id"]}))
        .await
        .unwrap();
    next(&mut rx, "turn_finished").await;
    assert_eq!(a.calls.lock().unwrap().len(), 1);
    let detail = project_call(&host, "work_get", json!({"work":receipt["work"]}))
        .await
        .unwrap();
    assert_eq!(detail["template"]["objective"], "read the attachment");
    assert_eq!(detail["template"]["artifacts"][0], artifact.id.0);
    assert_eq!(detail["usage"].as_array().unwrap().len(), 1);
    assert_eq!(detail["usage"][0]["usage"]["model_calls"], 1);
    assert_eq!(detail["usage"][0]["usage"]["cost_micro_usd"], Value::Null);
    let recorded = serde_json::to_string(&a.calls.lock().unwrap()[0].messages).unwrap();
    assert!(recorded.contains(&artifact.id.0));
    assert!(recorded.contains("brief.txt"));
    let follow = project_call(
        &host,
        "work_admit",
        json!({"request_id":"two",
        "target":{"kind":"general","model":"b"},
        "task":{"objective":"continue with B","item":receipt["work"]}}),
    )
    .await
    .unwrap();
    assert_eq!(follow["conversation"], receipt["conversation"]);
    project_call(&host, "work_execute", json!({"receipt":follow["id"]}))
        .await
        .unwrap();
    next(&mut rx, "turn_finished").await;
    assert_eq!(b.calls.lock().unwrap().len(), 1);
    assert_eq!(a.calls.lock().unwrap().len(), 1);
    project_call(&host, "work_execute", json!({"receipt":follow["id"]}))
        .await
        .unwrap();
    assert_eq!(b.calls.lock().unwrap().len(), 1);
    let template = project_call(
        &host,
        "work_get",
        json!({"work":receipt["work"],"receipt":follow["id"]}),
    )
    .await
    .unwrap();
    assert_eq!(template["template"]["objective"], "read the attachment");
    assert_eq!(template["template"]["artifacts"][0], artifact.id.0);
    assert_eq!(template["template"]["target"]["model"], "a");
    let list = project_call(&host, "work_list", json!({})).await.unwrap();
    assert_eq!(list["items"][0]["state"], "completed");
    project_call(&host, "work_archive", json!({"work":receipt["work"]}))
        .await
        .unwrap();
    assert!(
        project_call(&host, "work_list", json!({})).await.unwrap()["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        project_call(&host, "work_list", json!({"archived":true}))
            .await
            .unwrap()["items"][0]["id"],
        receipt["work"]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn work_questions_keep_their_conversation_and_reads_do_not_interrupt_the_turn() {
    use ark_interact::ark_orchestrate::{Models, Orchestrator};
    let dir = tempfile::tempdir().unwrap();
    let saves = Arc::new(AtomicUsize::new(0));
    let provider = mock("notes.save");
    let ark = Ark::builder()
        .store_root(dir.path())
        .provider(provider.clone())
        .tool(Arc::new(Notes {
            saves: saves.clone(),
        }))
        .profile(AgentProfile::builder().core_actions(["notes.save"]).build())
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.create_project("work", "test").unwrap();
    let o = Orchestrator::builder(&ark, &project)
        .models(Arc::new(
            Models::builder()
                .default_model("test")
                .provider("test", provider)
                .build(),
        ))
        .build()
        .unwrap();
    let app = App::new("work approvals")
        .ark(ark)
        .project(move |_| Ok(project.clone()))
        .orchestrator(o);
    let host = ArkUi::new(app).open().await.unwrap().0;
    let receipt = project_call(
        &host,
        "work_admit",
        json!({"request_id":"one",
        "target":{"kind":"general","model":null},"task":{"objective":"save these notes"}}),
    )
    .await
    .unwrap();
    let other = project_call(
        &host,
        "work_admit",
        json!({"request_id":"two",
        "target":{"kind":"general","model":null},"task":{"objective":"another work"}}),
    )
    .await
    .unwrap();
    project_call(&host, "work_cancel", json!({"work":other["work"]}))
        .await
        .unwrap();
    let cancelled = project_call(&host, "work_get", json!({"work":other["work"]}))
        .await
        .unwrap();
    assert_eq!(cancelled["item"]["state"], "cancelled");
    assert!(cancelled["item"]["start_receipt"].is_null());
    project_call(&host, "work_execute", json!({"receipt":other["id"]}))
        .await
        .unwrap();
    assert_eq!(
        project_call(&host, "work_get", json!({"work":other["work"]}))
            .await
            .unwrap()["item"]["state"],
        "cancelled"
    );
    let pending = project_call(&host, "work_get", json!({"work":receipt["work"]}))
        .await
        .unwrap();
    assert_eq!(pending["item"]["start_receipt"], receipt["id"]);
    let mut rx = host.subscribe();
    project_call(&host, "work_execute", json!({"receipt":receipt["id"]}))
        .await
        .unwrap();
    let ask = next(&mut rx, "approval").await;
    let current = project_call(&host, "work_get", json!({"work":receipt["work"]}))
        .await
        .unwrap();
    assert_eq!(current["item"]["state"], "attention");
    assert_eq!(current["item"]["questions"][0]["id"], ask["id"]);
    assert!(project_call(
        &host,
        "work_answer",
        json!({"work":other["work"],"id":ask["id"],"decision":"allow_once"})
    )
    .await
    .is_err());
    assert_eq!(saves.load(Ordering::SeqCst), 0);
    project_call(
        &host,
        "work_answer",
        json!({"work":receipt["work"],"id":ask["id"],"decision":"allow_once"}),
    )
    .await
    .unwrap();
    let end = next(&mut rx, "turn_finished").await;
    assert_eq!(end["ok"], true, "{end}");
    assert_eq!(saves.load(Ordering::SeqCst), 1);
    let finished = project_call(&host, "work_get", json!({"work":receipt["work"]}))
        .await
        .unwrap();
    assert_eq!(finished["item"]["state"], "completed");
    assert_eq!(finished["transcript"][0]["interrupted"], Value::Null);
    assert!(project_call(
        &host,
        "work_answer",
        json!({"work":receipt["work"],"id":ask["id"],"decision":"allow_once"})
    )
    .await
    .is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn signed_webhook_http_preview_delivery_and_rotation_use_real_transport_without_model_calls()
{
    use ark_interact::ark_orchestrate::{Models, Orchestrator};
    use base64::Engine as _;
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(MockProvider::new("test", vec![]));
    let ark = Ark::builder()
        .store_root(dir.path())
        .provider(provider.clone())
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.create_project("webhook protocol", "test").unwrap();
    let o = Orchestrator::builder(&ark, &project)
        .models(Arc::new(
            Models::builder()
                .default_model("test")
                .provider("test", provider.clone())
                .build(),
        ))
        .build()
        .unwrap();
    let chosen = project.clone();
    let app = App::new("Webhook protocol test")
        .ark(ark)
        .project(move |_| Ok(chosen.clone()))
        .orchestrator(o.clone());
    let (host, served) = ArkUi::new(app)
        .serve_background("127.0.0.1:0")
        .await
        .unwrap();
    let draft = json!({"name":"Retain verified input","trigger":{"kind":"webhook"},"inputs":[{"name":"message","port":"text","required":true}],"steps":[{"id":"result","name":"Result","action":{"kind":"result","inputs":[{"kind":"input","name":"message","port":"text"}]}}]});
    let saved = project_call(
        &host,
        "automation_save",
        json!({"request":"definition","draft":draft}),
    )
    .await
    .unwrap();
    let automation = saved["automation"].as_str().unwrap();
    let base = format!("http://127.0.0.1:{}/", served.port);
    let setup=project_call(&host,"webhook_setup",json!({"automation":automation,"expected":1,"request":"connection-one","base":base,"max_per_hour":5})).await.unwrap();
    let endpoint = setup["receipt"]["endpoint"].as_str().unwrap().to_string();
    let connection = setup["receipt"]["connection"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let secret = setup["secret"].as_str().unwrap().to_string();
    assert!(project_call(
        &host,
        "automation_prepare",
        json!({"automation":automation,"expected":1})
    )
    .await
    .is_err());
    let verified = project_call(
        &host,
        "webhook_probe",
        json!({"endpoint":endpoint,"connection":connection}),
    )
    .await
    .unwrap();
    assert_eq!(verified["draft"]["availability"], "local");
    assert!(o
        .work_page("person", false, None, 50)
        .unwrap()
        .entries
        .is_empty());
    assert_eq!(provider.calls.lock().unwrap().len(), 0);
    let grant = project_call(
        &host,
        "automation_prepare",
        json!({"automation":automation,"expected":1}),
    )
    .await
    .unwrap();
    project_call(
        &host,
        "automation_activate",
        json!({"automation":automation,"activation":grant["id"],"digest":grant["digest"]}),
    )
    .await
    .unwrap();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let oversized = client
        .post(format!("{base}webhooks/v1/{endpoint}"))
        .body(vec![b'x'; 64 * 1024 + 1])
        .send()
        .await
        .unwrap();
    assert_eq!(oversized.status(), reqwest::StatusCode::PAYLOAD_TOO_LARGE);
    {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut socket = tokio::net::TcpStream::connect(("127.0.0.1", served.port))
            .await
            .unwrap();
        socket.write_all(format!("POST /webhooks/v1/{endpoint} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 1\r\n\r\n").as_bytes()).await.unwrap();
        let mut response = Vec::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(35),
            socket.read_to_end(&mut response),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(response.starts_with(b"HTTP/1.1 408 Request Timeout"));
    }
    let send = |connection: String, secret: String, event: &str, body: Vec<u8>| {
        let client = client.clone();
        let endpoint = endpoint.clone();
        let event = event.to_string();
        let base = base.clone();
        async move {
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
                .to_string();
            let mut bytes = format!(
                "ark-webhook-v1\n{endpoint}\n{connection}\ndeliver\n{timestamp}\n{event}\n"
            )
            .into_bytes();
            bytes.extend_from_slice(&body);
            let signature = ring::hmac::sign(
                &ring::hmac::Key::new(ring::hmac::HMAC_SHA256, secret.as_bytes()),
                &bytes,
            );
            client
                .post(format!("{base}webhooks/v1/{endpoint}"))
                .header("x-ark-connection", connection)
                .header("x-ark-mode", "deliver")
                .header("x-ark-timestamp", timestamp)
                .header("x-ark-event", event)
                .header(
                    "x-ark-signature",
                    format!(
                        "v1={}",
                        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signature.as_ref())
                    ),
                )
                .header("content-type", "application/json")
                .body(body)
                .send()
                .await
                .unwrap()
        }
    };
    let body = serde_json::to_vec(
        &json!({"inputs":{"message":{"kind":"text","value":"verified source text"}}}),
    )
    .unwrap();
    let response = send(
        connection.clone(),
        secret.clone(),
        "event-one",
        body.clone(),
    )
    .await;
    assert_eq!(response.status(), 202);
    let accepted: Value = response.json().await.unwrap();
    assert_eq!(accepted["duplicate"], false);
    let run = accepted["run"].as_str().unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let state = o.automation_run(run).unwrap();
            if state.phase.is_final() {
                assert_eq!(
                    state.phase,
                    ark_interact::ark_orchestrate::automation::RunPhase::Completed
                );
                assert_eq!(state.output.text, "verified source text");
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let duplicate: Value = send(
        connection.clone(),
        secret.clone(),
        "event-one",
        body.clone(),
    )
    .await
    .json()
    .await
    .unwrap();
    assert_eq!(duplicate["run"], run);
    assert_eq!(duplicate["duplicate"], true);
    let wrong = send(
        connection.clone(),
        "not-the-credential".into(),
        "invalid-signature",
        body.clone(),
    )
    .await;
    assert_eq!(wrong.status(), 401);
    let routing = serde_json::to_vec(&json!({"inputs":{},"project":"another-project"})).unwrap();
    assert_eq!(
        send(connection.clone(), secret.clone(), "bad-routing", routing)
            .await
            .status(),
        400
    );
    let url = format!("{base}webhooks/v1/{endpoint}");
    assert_eq!(
        client
            .post(&url)
            .bearer_auth("a-native-device-token-is-not-a-webhook-key")
            .body(body.clone())
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let replacement=project_call(&host,"webhook_setup",json!({"automation":automation,"expected":1,"request":"connection-two","base":base,"max_per_hour":5})).await.unwrap();
    let replacement_connection = replacement["receipt"]["connection"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let replacement_secret = replacement["secret"].as_str().unwrap().to_string();
    assert_eq!(replacement["receipt"]["endpoint"], endpoint);
    assert_eq!(
        send(
            replacement_connection.clone(),
            replacement_secret.clone(),
            "pending-credential",
            body.clone()
        )
        .await
        .status(),
        401
    );
    project_call(
        &host,
        "webhook_probe",
        json!({"endpoint":endpoint,"connection":replacement_connection}),
    )
    .await
    .unwrap();
    let grant = project_call(
        &host,
        "automation_prepare",
        json!({"automation":automation,"expected":1}),
    )
    .await
    .unwrap();
    project_call(
        &host,
        "automation_activate",
        json!({"automation":automation,"activation":grant["id"],"digest":grant["digest"]}),
    )
    .await
    .unwrap();
    assert_eq!(
        send(connection, secret, "event-one", body.clone())
            .await
            .status(),
        401
    );
    let replay: Value = send(
        replacement_connection.clone(),
        replacement_secret.clone(),
        "event-one",
        body.clone(),
    )
    .await
    .json()
    .await
    .unwrap();
    assert_eq!(replay["run"], run);
    assert_eq!(replay["duplicate"], true);
    project_call(&host, "automation_pause", json!({"automation":automation}))
        .await
        .unwrap();
    let paused: Value = send(
        replacement_connection.clone(),
        replacement_secret.clone(),
        "event-one",
        body.clone(),
    )
    .await
    .json()
    .await
    .unwrap();
    assert_eq!(paused["run"], run);
    assert_eq!(
        send(
            replacement_connection,
            replacement_secret,
            "paused-new-event",
            body
        )
        .await
        .status(),
        409
    );
    assert_eq!(
        o.work_page("person", false, None, 50)
            .unwrap()
            .entries
            .len(),
        1
    );
    assert_eq!(provider.calls.lock().unwrap().len(), 0);
    assert!(o.agents().is_empty());
    let visible = project_call(&host, "work_get", json!({"work":accepted["work"]}))
        .await
        .unwrap();
    assert_eq!(visible["item"]["state"], "completed");
    assert_eq!(visible["transcript"][0]["answer"], "verified source text");
    let logs = project_call(&host, "webhook_get", json!({"automation":automation}))
        .await
        .unwrap();
    assert!(logs["deliveries"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["state"] == "duplicate"));
    assert!(!logs.to_string().contains(setup["secret"].as_str().unwrap()));
}

#[tokio::test(flavor = "multi_thread")]
async fn default_general_followup_reuses_the_original_target_and_private_conversation() {
    use ark_interact::ark_orchestrate::{Models, Orchestrator};
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(MockProvider::new(
        "test",
        ["First", "Followup"]
            .into_iter()
            .map(|text| Completion {
                content: text.into(),
                tool_calls: vec![],
                usage: Default::default(),
            })
            .collect(),
    ));
    let ark = Ark::builder()
        .store_root(dir.path())
        .provider(provider.clone())
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.create_project("default General", "").unwrap();
    let o = Orchestrator::builder(&ark, &project)
        .models(Arc::new(
            Models::builder()
                .default_model("test")
                .provider("test", provider.clone())
                .build(),
        ))
        .build()
        .unwrap();
    let app = App::new("General followup")
        .ark(ark)
        .project(move |_| Ok(project.clone()))
        .orchestrator(o.clone());
    let host = ArkUi::new(app).open().await.unwrap().0;
    let first = project_call(&host, "work_admit", json!({"request_id":"first","target":{"kind":"general","model":null},"task":{"objective":"Start"}})).await.unwrap();
    let mut events = host.subscribe();
    project_call(&host, "work_execute", json!({"receipt":first["id"]}))
        .await
        .unwrap();
    next(&mut events, "turn_finished").await;
    let detail = project_call(&host, "work_get", json!({"work":first["work"]}))
        .await
        .unwrap();
    assert_eq!(
        detail["item"]["target"],
        json!({"kind":"general","model":null})
    );
    let follow = project_call(&host,"work_admit",json!({"request_id":"follow","receipt":detail["active_receipt"],"target":detail["item"]["target"],"task":{"objective":"Continue","item":first["work"]}})).await.unwrap();
    assert_eq!(follow["conversation"], first["conversation"]);
    project_call(&host, "work_execute", json!({"receipt":follow["id"]}))
        .await
        .unwrap();
    next(&mut events, "turn_finished").await;
    let detail = project_call(&host, "work_get", json!({"work":first["work"]}))
        .await
        .unwrap();
    assert_eq!(detail["transcript"].as_array().unwrap().len(), 2);
    assert_eq!(detail["conversations"].as_array().unwrap().len(), 1);
    assert_eq!(provider.calls.lock().unwrap().len(), 2);
    assert!(o.agents().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn another_host_reads_each_scheduled_conversation_and_stop_only_cancels_the_selected_one() {
    use ark_interact::ark_orchestrate::{
        AgentSpec, Models, Orchestrator, ScheduleChange, TaskSpec, TaskState, When,
    };
    use ark_sdk::prelude::ark_model::{Message, Provider, ToolDef};
    struct Held {
        release: Arc<tokio::sync::Notify>,
        calls: Arc<AtomicUsize>,
    }
    #[async_trait::async_trait]
    impl Provider for Held {
        fn host_controls_tools(&self) -> bool {
            true
        }
        fn describe(&self) -> String {
            "held test".into()
        }
        async fn complete(
            &self,
            _: &str,
            _: &[Message],
            _: &[ToolDef],
        ) -> ark_model::Result<Completion> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.release.notified().await;
            Ok(Completion {
                content: "First completed".into(),
                tool_calls: vec![],
                usage: Default::default(),
            })
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let release = Arc::new(tokio::sync::Notify::new());
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = Arc::new(Held {
        release: release.clone(),
        calls: calls.clone(),
    });
    let ark = Ark::builder()
        .store_root(dir.path())
        .provider(provider.clone())
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.create_project("shared scheduled item", "").unwrap();
    let models = Arc::new(
        Models::builder()
            .default_model("test")
            .provider("test", provider)
            .build(),
    );
    let owner = Orchestrator::builder(&ark, &project)
        .models(models.clone())
        .build()
        .unwrap();
    owner
        .spawn(AgentSpec::new("reviewer").model("test"))
        .await
        .unwrap();
    let observer = Orchestrator::builder(&ark, &project)
        .models(models)
        .build()
        .unwrap();
    let make_app = |o, ark: Ark| {
        let project = project.clone();
        App::new("scheduled selection")
            .ark(ark)
            .project(move |_| Ok(project.clone()))
            .orchestrator(o)
    };
    let owner_host = ArkUi::new(make_app(owner.clone(), ark.clone()))
        .open()
        .await
        .unwrap()
        .0;
    let observer_host = ArkUi::new(make_app(observer, ark)).open().await.unwrap().0;
    let item = owner
        .add_work("person", "Repeated report", Default::default())
        .unwrap();
    let mut task = TaskSpec::new("Report");
    task.item = Some(item.id.clone());
    let schedule = owner
        .schedule("reviewer", task, When::every_minutes(5))
        .unwrap();
    owner
        .change_schedule(
            &schedule.id,
            ScheduleChange {
                overlap: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
    let first = owner
        .run_schedule_now_with_key(&schedule.id, "first")
        .await
        .unwrap();
    let second = owner
        .run_schedule_now_with_key(&schedule.id, "second")
        .await
        .unwrap();
    let snapshot = owner.work_detail("person", &item.id).unwrap();
    let receipts = &snapshot.entries[0].receipts;
    let one = receipts
        .iter()
        .find(|receipt| receipt.task.as_ref() == Some(&first))
        .unwrap();
    let two = receipts
        .iter()
        .find(|receipt| receipt.task.as_ref() == Some(&second))
        .unwrap();
    for receipt in [one, two] {
        let detail = project_call(
            &observer_host,
            "work_get",
            json!({"work":item.id,"receipt":receipt.id}),
        )
        .await
        .unwrap();
        assert_eq!(detail["item"]["conversation"], receipt.conversation);
        assert_eq!(
            detail["item"]["target"],
            json!({"kind":"agent","name":"reviewer"})
        );
        assert_eq!(detail["item"]["tasks"].as_array().unwrap().len(), 1);
    }
    project_call(
        &owner_host,
        "work_cancel",
        json!({"work":item.id,"receipt":two.id}),
    )
    .await
    .unwrap();
    let cancelled = tokio::time::timeout(Duration::from_secs(3), owner.wait(&second))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cancelled.state, TaskState::Cancelled);
    assert!(!owner.task(&first).unwrap().state.is_final());
    release.notify_one();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), owner.wait(&first))
            .await
            .unwrap()
            .unwrap()
            .state,
        TaskState::Done
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn every_manual_start_client_observes_the_last_receipt_before_repeating() {
    use ark_interact::ark_orchestrate::{Models, Orchestrator};
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(MockProvider::new("test", vec![]));
    let ark = Ark::builder()
        .store_root(dir.path())
        .provider(provider.clone())
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.create_project("manual receipts", "test").unwrap();
    let o = Orchestrator::builder(&ark, &project)
        .models(Arc::new(
            Models::builder()
                .default_model("test")
                .provider("test", provider)
                .build(),
        ))
        .build()
        .unwrap();
    let draft = serde_json::from_value(json!({"name":"Retained start","trigger":{"kind":"manual"},"inputs":[],"authority":{"actions":[],"max_calls":8,"max_mutations":0},"steps":[{"id":"result","name":"Result","action":{"kind":"result","inputs":[{"kind":"literal","text":"retained"}]}}]})).unwrap();
    let saved = o.save_automation("create", None, None, draft).unwrap();
    let host = ArkUi::new(
        App::new("manual receipts")
            .ark(ark)
            .project(move |_| Ok(project.clone()))
            .orchestrator(o),
    )
    .open()
    .await
    .unwrap()
    .0;
    let start = json!({"automation":saved.automation,"expected":1,"request":"direct-client","inputs":{},"previous":null});
    let first = project_call(&host, "automation_run", start.clone())
        .await
        .unwrap();
    let receipt = project_call(&host, "automation_acknowledge_start", start)
        .await
        .unwrap();
    assert_eq!(receipt["run"], first["id"]);
    let restored = project_call(
        &host,
        "automation_get",
        json!({"automation":saved.automation}),
    )
    .await
    .unwrap();
    assert_eq!(restored["entry"]["last_start"]["run"], first["id"]);
    assert!(project_call(&host, "automation_run", json!({"automation":saved.automation,"expected":1,"request":"stale-editor","inputs":{},"previous":null})).await.is_err());
    let repeat = project_call(&host, "automation_run", json!({"automation":saved.automation,"expected":1,"request":"explicit-repeat","inputs":{},"previous":first["id"]})).await.unwrap();
    assert_ne!(repeat["id"], first["id"]);
    let pending = project_call(
        &host,
        "automation_get",
        json!({"automation":saved.automation}),
    )
    .await
    .unwrap();
    assert_eq!(
        pending["entry"]["pending_start"]["request"],
        "explicit-repeat"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn webhook_ingress_limits_invalid_signatures_and_slow_bodies_without_blocking_rpc() {
    use tokio::io::AsyncWriteExt;
    let dir = tempfile::tempdir().unwrap();
    use ark_interact::ark_orchestrate::{Models, Orchestrator};
    let model = Arc::new(MockProvider::new("test", vec![]));
    let ark = Ark::builder()
        .store_root(dir.path())
        .provider(model.clone())
        .build()
        .unwrap();
    let project = ark.create_project("ingress limits", "test").unwrap();
    let orchestrator = Orchestrator::builder(&ark, &project)
        .models(Arc::new(
            Models::builder()
                .default_model("test")
                .provider("test", model.clone())
                .build(),
        ))
        .build()
        .unwrap();
    let app = App::new("Ingress limits")
        .ark(ark)
        .project(move |_| Ok(project.clone()))
        .orchestrator(orchestrator);
    let (host, served) = ArkUi::new(app)
        .serve_background("127.0.0.1:0")
        .await
        .unwrap();
    let base = format!("http://127.0.0.1:{}/", served.port);
    let saved = project_call(&host, "automation_save", json!({"request":"ingress-definition","draft":{"name":"Ingress test","trigger":{"kind":"webhook"},"steps":[{"id":"result","name":"Result","action":{"kind":"result","inputs":[]}}]}})).await.unwrap();
    let preview = project_call(
        &host,
        "automation_preview_inputs",
        json!({"automation":saved["automation"],"expected":1,"inputs":{}}),
    )
    .await
    .unwrap();
    assert_eq!(preview["revision"], 1);
    assert_eq!(preview["bindings"], json!([]));
    let oversized = project_call(&host, "automation_preview_inputs", json!({"automation":saved["automation"],"expected":1,"inputs":{"large":{"kind":"text","value":"x".repeat(64*1024)}}})).await.unwrap_err().to_string();
    assert!(oversized.contains("64 KiB"));
    let validation = project_call(&host, "automation_validate", json!({"draft":{"name":"Required scheduled input","trigger":{"kind":"schedule","when":{"at":"09:00","tz":"UTC"}},"inputs":[{"name":"message","port":"text","required":true}],"steps":[{"id":"result","name":"Result","action":{"kind":"result","inputs":[]}}]}})).await.unwrap();
    assert_eq!(validation["can_save"], true);
    assert_eq!(validation["issues"], json!([]));
    assert_eq!(
        validation["activation_issues"][0]["field"],
        "Execution inputs"
    );
    assert!(project_call(
        &host,
        "automation_history",
        json!({"automation":saved["automation"],"limit":10})
    )
    .await
    .unwrap()["runs"]
        .as_array()
        .unwrap()
        .is_empty());
    let setup = project_call(&host, "webhook_setup", json!({"automation":saved["automation"],"expected":1,"request":"ingress-key","base":base,"max_per_hour":5})).await.unwrap();
    let endpoint = setup["receipt"]["endpoint"].as_str().unwrap();
    let connection = setup["receipt"]["connection"]["id"].as_str().unwrap();
    let url = format!("{base}webhooks/v1/{endpoint}");
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(3))
        .build()
        .unwrap();
    let invalid = || {
        client
            .post(&url)
            .header("x-ark-mode", "preview")
            .header("x-ark-connection", connection)
            .header(
                "x-ark-timestamp",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis()
                    .to_string(),
            )
            .header("x-ark-event", "invalid-ingress")
            .header("x-ark-signature", format!("v1={}", "0".repeat(64)))
            .body("{}")
    };
    let mut throttled = 0;
    for _ in 0..40 {
        let response = invalid().send().await.unwrap();
        match response.status() {
            reqwest::StatusCode::UNAUTHORIZED => {}
            reqwest::StatusCode::TOO_MANY_REQUESTS => {
                throttled += 1;
                assert_eq!(response.headers()["retry-after"], "1");
            }
            status => panic!("unexpected invalid signature response: {status}"),
        }
    }
    assert!(throttled > 0);
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        invalid().send().await.unwrap().status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let mut slow = Vec::new();
    for _ in 0..8 {
        let mut socket = tokio::net::TcpStream::connect(("127.0.0.1", served.port))
            .await
            .unwrap();
        socket.write_all(format!("POST /webhooks/v1/{endpoint} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 1\r\n\r\n").as_bytes()).await.unwrap();
        slow.push(socket);
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        invalid().send().await.unwrap().status(),
        reqwest::StatusCode::TOO_MANY_REQUESTS
    );
    let served_url = reqwest::Url::parse(&served.url).unwrap();
    let token = served_url
        .query_pairs()
        .find(|(key, _)| key == "token")
        .unwrap()
        .1
        .into_owned();
    let summary = client
        .post(format!("{base}rpc"))
        .bearer_auth(&token)
        .json(&json!({"method":"summary","params":{}}))
        .send()
        .await
        .unwrap();
    assert_eq!(summary.status(), reqwest::StatusCode::OK);
    assert_eq!(summary.json::<Value>().await.unwrap()["ok"], true);
    drop(slow);
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        invalid().send().await.unwrap().status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    assert_eq!(model.calls.lock().unwrap().len(), 0);
}

struct HeldCommit {
    started: Arc<tokio::sync::Notify>,
    release: std::sync::Mutex<Option<std::sync::mpsc::Receiver<()>>>,
}

#[async_trait::async_trait]
impl Adapter for HeldCommit {
    fn name(&self) -> &str {
        "held"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![ActionDescriptor::new(
            "held.write",
            "write",
            RiskTier::Mutate,
        )]
    }
    async fn run(&self, _: &str, _: Value, ctx: &RunCtx<'_>) -> ark_tools::Result<ActionOutput> {
        let fence = ctx.commit_fence.clone().expect("turn fence");
        let release = self.release.lock().unwrap().take().unwrap();
        let started = self.started.clone();
        tokio::task::spawn_blocking(move || {
            fence.if_active(&mut || {
                started.notify_one();
                release.recv().unwrap();
            })
        })
        .await
        .unwrap();
        Ok(ActionOutput::json(json!({})))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn overlapping_stops_and_new_turn_wait_for_a_held_commit() {
    let dir = tempfile::tempdir().unwrap();
    let started = Arc::new(tokio::sync::Notify::new());
    let (release, receive) = std::sync::mpsc::channel();
    let provider: Arc<dyn ark_model::Provider> = mock("held.write");
    let ark = Ark::builder()
        .store_root(dir.path())
        .tool(Arc::new(HeldCommit {
            started: started.clone(),
            release: std::sync::Mutex::new(Some(receive)),
        }))
        .profile(AgentProfile::builder().core_actions(["held.write"]).build())
        .provider(provider)
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let host = ArkUi::new(App::new("Test").ark(ark))
        .open()
        .await
        .unwrap()
        .0;
    let mut events = host.subscribe();
    host.call("send", json!({"text":"write"})).await.unwrap();
    let ask = next(&mut events, "approval").await;
    host.call("approve", json!({"id":ask["id"],"decision":"allow_once"}))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), started.notified())
        .await
        .unwrap();
    let mut first = Box::pin(host.call("cancel", json!({})));
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(
            std::future::Future::poll(first.as_mut(), cx).is_pending()
        ))
        .await
    );
    let mut second = Box::pin(host.call("cancel", json!({})));
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(
            std::future::Future::poll(second.as_mut(), cx).is_pending()
        ))
        .await
    );
    assert!(host.call("send", json!({"text":"new turn"})).await.is_err());
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(10), first)
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), second)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(next(&mut events, "turn_finished").await["cancelled"], true);
}

struct FallibleLifecycle {
    stop_calls: Arc<AtomicUsize>,
}
#[async_trait::async_trait]
impl Adapter for FallibleLifecycle {
    fn name(&self) -> &str {
        "lifecycle"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![ActionDescriptor::new(
            "lifecycle.read",
            "read",
            RiskTier::Read,
        )]
    }
    async fn run(&self, _: &str, _: Value, ctx: &RunCtx<'_>) -> ark_tools::Result<ActionOutput> {
        let fence = ctx.commit_fence.clone().unwrap();
        let calls = self.stop_calls.clone();
        tokio::task::spawn_blocking(move || {
            let binding = Arc::new(ark_tools::TurnLifecycle {
                key: "fixture".into(),
                initialize: Arc::new(|| Ok(())),
                on_finish: Arc::new(|| Err("injected finish failure".into())),
                on_stop: Arc::new(move || {
                    if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                        Err("injected stop failure".into())
                    } else {
                        Ok(())
                    }
                }),
            });
            assert!(fence.register_lifecycle(binding).unwrap());
        })
        .await
        .unwrap();
        Ok(ActionOutput::json(json!({})))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn lifecycle_failure_reports_retry_and_the_chat_recovers() {
    let dir = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let ark = Ark::builder()
        .store_root(dir.path())
        .tool(Arc::new(FallibleLifecycle {
            stop_calls: calls.clone(),
        }))
        .profile(
            AgentProfile::builder()
                .core_actions(["lifecycle.read"])
                .build(),
        )
        .provider(mock("lifecycle.read"))
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let host = ArkUi::new(App::new("Test").ark(ark))
        .open()
        .await
        .unwrap()
        .0;
    let mut events = host.subscribe();
    host.call("send", json!({"text":"read"})).await.unwrap();
    let retry = next(&mut events, "turn_cleanup").await;
    assert_eq!(retry["message"], "Could not finish this turn. Retrying.");
    let ended = next(&mut events, "turn_finished").await;
    assert_eq!(ended["ok"], false);
    assert!(calls.load(Ordering::SeqCst) >= 2);
    host.call("send", json!({"text":"another turn"}))
        .await
        .unwrap();
}
