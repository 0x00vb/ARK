//! The real page, in WebKit, against a host with a scripted mock model. These guard rendering
//! bugs found while dogfooding: a dropped last chunk, a doubled answer, steering placement,
//! live and replayed turns drawing differently, and a narrow window hiding the sidebar.
//!
//! They need Node and a browser, so they run only when asked:
//!
//! ```text
//! cd crates/ark-ui/tests/web && npm ci && npx playwright-core install webkit
//! ARK_UI_PAGE_TESTS=1 cargo test -p ark-ui --test page
//! ```

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ark_interact::App;
use ark_sdk::prelude::ark_model::{Completion, MockProvider, ToolCall, Usage};
use ark_sdk::prelude::*;
use ark_ui::ArkUi;
use serde_json::{json, Value};

struct Notes;

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
    async fn run(&self, _a: &str, _i: Value, _c: &RunCtx<'_>) -> ark_tools::Result<ActionOutput> {
        tokio::time::sleep(Duration::from_millis(300)).await;
        Ok(ActionOutput::json(
            json!({ "text": "the locket, the key, the door" }),
        ))
    }
}

fn say(text: &str, tool: bool) -> Completion {
    let calls = if tool {
        vec![ToolCall {
            id: "c1".into(),
            name: "notes.read".into(),
            arguments: json!({}),
        }]
    } else {
        vec![]
    };
    Completion {
        content: text.into(),
        tool_calls: calls,
        usage: Usage::default(),
    }
}

fn web_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/web")
}

/// Serve an app whose model follows `script`, then drive `scenario` in the page.
async fn drive(scenario: &str, script: Vec<Completion>, chunk: Duration) {
    drive_app(scenario, script, chunk, false).await
}

/// `drive`, with an orchestrator per project when `agents` is set.
async fn drive_app(scenario: &str, script: Vec<Completion>, chunk: Duration, agents: bool) {
    if std::env::var_os("ARK_UI_PAGE_TESTS").is_none() {
        eprintln!("skipped: set ARK_UI_PAGE_TESTS=1 to run the page tests (see the file header)");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let model: Arc<dyn ark_model::Provider> =
        Arc::new(MockProvider::new("m", script).with_chunk_delay(chunk));
    let ark = Ark::builder()
        .store_root(dir.path())
        .tool(Arc::new(Notes))
        .profile(AgentProfile::builder().core_actions(["notes.read"]).build())
        .provider(model)
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let mut app = App::new("Page test").ark(ark);
    if agents {
        app = app.orchestrate(|ark, project, gate| {
            Ok(
                ark_interact::ark_orchestrate::Orchestrator::builder(ark, project)
                    .approvals(gate)
                    .build()?,
            )
        });
    }
    let (_host, served) = ArkUi::new(app)
        .serve_background("127.0.0.1:0")
        .await
        .unwrap();
    let out = tokio::process::Command::new("node")
        .arg("page.mjs")
        .arg(scenario)
        .arg(&served.url)
        .current_dir(web_dir())
        .output()
        .await
        .expect("node is on PATH");
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{scenario} failed:\n{text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn streamed_text_keeps_its_last_chunk_and_answers_once() {
    let script = vec![
        say("Reading the notes now.", true),
        say("The notes mention a locket.", false),
    ];
    drive("stream", script, Duration::from_millis(40)).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_row_after_the_streamed_answer_does_not_repeat_it() {
    // An unknown tool cites nothing, so the self-check flags the answer after it streamed.
    let mut unknown = say("", true);
    unknown.tool_calls[0].name = "notes.missing".into();
    let script = vec![unknown, say("The notes mention a locket.", false)];
    drive("selfcheck", script, Duration::from_millis(40)).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stopped_turn_replays_as_it_was_shown() {
    let long =
        "Reading the notes slowly, one line at a time, so that there is time to stop. ".repeat(4);
    drive(
        "cancel",
        vec![say(&long, true), say("unused", false)],
        Duration::from_millis(120),
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_plus_menu_opens_files_and_a_terminal_in_the_project_folder() {
    drive("workbench", vec![], Duration::ZERO).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn settings_sections_show_and_compose_fills_the_composer_unsent() {
    drive("settings", vec![], Duration::ZERO).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_narrow_window_keeps_the_sidebar_reachable() {
    drive("narrow", vec![], Duration::ZERO).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_model_menu_lists_backends_and_switches() {
    // Stand-in CLIs: Claude is signed in, Codex is not installed. Nothing here talks to a vendor.
    let bin = tempfile::tempdir().unwrap();
    let claude = bin.path().join("claude");
    std::fs::write(
        &claude,
        "#!/bin/sh\necho '{\"loggedIn\": true, \"authMethod\": \"claude.ai\"}'\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::env::set_var("ARK_CLAUDE_BIN", &claude);
    std::env::set_var("ARK_CODEX_BIN", bin.path().join("no-codex"));
    drive("models", vec![], Duration::ZERO).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn steering_lands_where_it_was_read_and_replays_the_same() {
    let script = vec![
        say(
            "Reading the notes first, then I will answer your question in full.",
            true,
        ),
        say("The notes mention a locket, and a key by the door.", false),
    ];
    drive("steer", script, Duration::from_millis(120)).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_window_that_reloads_mid_turn_rebuilds_it_once() {
    let narration =
        "Reading the notes slowly, one line at a time, so that a window can reload in between. "
            .repeat(3);
    let script = vec![
        say(narration.trim(), true),
        say("The notes mention a locket.", false),
    ];
    drive("rejoin", script, Duration::from_millis(120)).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failing_event_stream_backs_off_and_stops() {
    drive("backoff", vec![], Duration::ZERO).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_app_view_takes_the_main_column_and_agents_ask_from_the_tray() {
    let save = Completion {
        content: String::new(),
        tool_calls: vec![ToolCall {
            id: "c1".into(),
            name: "notes.save".into(),
            arguments: json!({}),
        }],
        usage: Usage::default(),
    };
    drive_app(
        "agents",
        vec![save, say("Saved.", false)],
        Duration::ZERO,
        true,
    )
    .await;
}
