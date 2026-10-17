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
//!
//! `ARK_UI_BROWSER=chromium` (after `npx playwright-core install chromium`) runs them in
//! Chromium, the engine behind WebView2 on Windows.

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

struct UnconfirmedEffect;

#[async_trait::async_trait]
impl Adapter for UnconfirmedEffect {
    fn name(&self) -> &str {
        "message"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![ActionDescriptor::new(
            "message.send",
            "submit a test message",
            RiskTier::Read,
        )]
    }
    async fn run(&self, _a: &str, _i: Value, _c: &RunCtx<'_>) -> ark_tools::Result<ActionOutput> {
        Ok(
            ActionOutput::json(json!({ "status": "submitted_unconfirmed" })).with_effect(
                ark_sdk::prelude::ark_core::ExternalEffect {
                    id: "page-test-message".into(),
                    state: ark_sdk::prelude::ark_core::ExternalEffectState::Unconfirmed,
                    retry_allowed: false,
                    user_message: Some(
                        "The message may have sent, but ARK could not confirm it.".into(),
                    ),
                },
            ),
        )
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

/// VM eligibility tests, separate from the real WebKit scenarios below.
#[test]
fn notification_bootstrap_contexts() {
    if std::env::var_os("ARK_UI_PAGE_TESTS").is_none() {
        return;
    }
    assert!(std::process::Command::new("node")
        .arg(web_dir().join("notification.mjs"))
        .status()
        .expect("node is on PATH")
        .success());
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
    if scenario == "skills" {
        let skill = dir.path().join("skills/release-notes");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            "---\nname: release-notes\ndescription: Write release notes from a list of changes\n---\nSteps.\n",
        )
        .unwrap();
    }
    let mut model: Arc<dyn ark_model::Provider> =
        Arc::new(MockProvider::new("m", script).with_chunk_delay(chunk));
    if scenario == "withdrawn" {
        model = Arc::new(CutOff {
            inner: model,
            cut: std::sync::atomic::AtomicBool::new(false),
        });
    }
    if scenario == "repair" || scenario == "unusable" {
        model = Arc::new(Unusable {
            inner: model,
            left: std::sync::atomic::AtomicUsize::new(if scenario == "repair" { 1 } else { 99 }),
        });
    }
    let actions = if scenario == "guarded-effect" {
        vec!["message.send"]
    } else {
        vec!["notes.read"]
    };
    let mut builder = Ark::builder()
        .store_root(dir.path())
        .tool(Arc::new(Notes))
        .tool(Arc::new(UnconfirmedEffect))
        .profile(AgentProfile::builder().core_actions(actions).build())
        .provider(model)
        .planner(false)
        .critic(false);
    if matches!(
        scenario,
        "capabilities" | "connect-mid-task" | "marketplace"
    ) {
        // Sign-in against a provider on loopback that approves at once; tokens in memory.
        let provider = fake_provider().await;
        let mut catalog = ark_connect::Catalog::builtin();
        let google = catalog
            .providers
            .iter_mut()
            .find(|p| p.id == "google")
            .unwrap();
        google.oauth.auth_url = format!("{provider}/auth");
        google.oauth.token_url = format!("{provider}/token");
        google.oauth.revoke_url = Some(format!("{provider}/revoke"));
        google.oauth.client_id = "page-test".into();
        google.oauth.client_id_env = None;
        let conns = ark_connect::Connections::open(
            dir.path(),
            catalog,
            Arc::new(ark_connect::MemorySecrets::default()),
        )
        .with_base("google.mail", &provider);
        builder = builder
            .connections_with(conns)
            .web_with(ark_tools::web::WebAdapter::new().backends(vec![]));
    }
    // A screen in memory for the `computer` scenario: what was pressed is checked after.
    let screen = ark_sdk::prelude::ark_computer::fake::FakeScreen::new()
        .app("Notes", &[("text field", "Title"), ("button", "Save")]);
    if scenario == "computer-switch" {
        builder = builder.computer_with(
            ark_sdk::prelude::ark_computer::ComputerService::with_screen(Arc::new(screen.clone()))
                .switched(dir.path().join("computer.json")),
        );
    } else if scenario.starts_with("computer") {
        builder = builder.computer_with(
            ark_sdk::prelude::ark_computer::ComputerService::with_screen(Arc::new(screen.clone())),
        );
    }
    if scenario == "processes" {
        // The test's own PATH, read as is (no login shell), records under the store.
        let path = std::env::var("PATH").unwrap_or_default();
        let shell = ark_sdk::prelude::ark_proc::Shell::fixed(
            "/bin/sh",
            vec![
                ("PATH".into(), path.into()),
                ("HOME".into(), dir.path().into()),
            ],
        );
        builder = builder.processes_with(
            ark_sdk::prelude::ark_proc::Processes::with_shell(dir.path().join("processes"), shell)
                .unwrap(),
        );
    }
    if scenario == "files" || scenario == "drop" {
        builder = builder.files(true);
    }
    if scenario == "analysis" || scenario == "image-failure" || scenario == "restore" {
        let Some(sandbox) = ark_sdk::prelude::ark_exec::Sandbox::native() else {
            eprintln!("skipped: no sandbox for runs on this platform");
            return;
        };
        builder = builder
            .files(true)
            .code_with(ark_sdk::prelude::ark_exec::Exec::with_sandbox(
                sandbox,
                dir.path().join("runs"),
                vec![Arc::new(Sh)],
            ));
    }
    let ark = builder.build().unwrap();
    let mut app = App::new("Page test").ark(ark);
    if scenario == "connect-mid-task" {
        // The person's browser: follow the sign-in page back to ARK, without a real browser.
        app = app.sign_in(Arc::new(|url: &str| {
            let url = url.to_string();
            std::thread::spawn(move || {
                if let Err(error) = follow(&url) {
                    eprintln!("test sign-in browser failed: {error}");
                }
            });
            true
        }));
    }
    if agents {
        app = app.orchestrate(|ark, project, gate| {
            Ok(
                ark_interact::ark_orchestrate::Orchestrator::builder(ark, project)
                    .approvals(gate)
                    .build()?,
            )
        });
    }
    let mut ui = ArkUi::new(app);
    if scenario == "accent" {
        // ARK Desktop's accent.
        ui = ui.accent("#335eb7");
    }
    if scenario == "file-viewer-consumer" {
        let Some(root) = std::env::var_os("ARK_UI_FILE_VIEWER_FIXTURE").map(PathBuf::from) else {
            eprintln!("skipped: ARK_UI_FILE_VIEWER_FIXTURE must name the consumer's ui folder");
            return;
        };
        let mut pending = vec![root.clone()];
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    if path.file_name().unwrap() != "tests" {
                        pending.push(path);
                    }
                } else {
                    let name = path.strip_prefix(&root).unwrap().to_str().unwrap();
                    ui = if name == "file-frame.html" {
                        ui.frame_asset(name, std::fs::read(&path).unwrap())
                    } else {
                        ui.asset(name, std::fs::read(&path).unwrap())
                    };
                }
            }
        }
        ui = ui
            .asset(
                "file-proof-entry.js",
                br#"import './file-renderers.js'; window.ark.extensionReady('file-proof-entry');"#
                    .as_slice(),
            )
            .extension_script("file-proof-entry.js");
    }
    if scenario == "advanced-files" {
        // The same worker as a registered WASM worker and as an ordinary asset.
        const PROBE: &[u8] = br#"// Each result is true or the name of the error that refused it.
const attempt = action => action().then(() => true, error => error.name);
const compiled = await attempt(() => WebAssembly.compile(new Uint8Array([0, 97, 115, 109, 1, 0, 0, 0])));
const fetched = await attempt(() => fetch('/ext/plain-probe.js'));
postMessage({ compiled, fetched });"#;
        ui = ui
            .wasm_worker_asset("wasm-probe.js", PROBE)
            .asset("plain-probe.js", PROBE);
    }
    if scenario == "ready" {
        ui = ui
            .asset(
                "ready-app.js",
                br#"
window.readyProbe = { calls: 0, wakes: 0, before: window.ark.summary() };
const summary = await window.ark.ready();
window.readyProbe.wakes += 1;
window.readyProbe.after = summary;
window.readyProbe.calls += 1;
window.readyProbe.graph = await window.ark.call("graph");
"#
                .as_slice(),
            )
            .script("ready-app.js");
    }
    if scenario == "startup-extension-failure" {
        ui = ui
            .asset(
                "broken.js",
                br#"import "./missing-transitive.js";
window.ark.extensionReady("broken");"#
                    .as_slice(),
            )
            .extension_script("broken.js");
    }
    if scenario == "late-extension" {
        ui = ui
            .asset(
                "late.js",
                br#"await new Promise((resolve) => setTimeout(resolve, 120));
window.ark.extensionReady("late");"#
                    .as_slice(),
            )
            .extension_script("late.js");
    }
    let (host, served) = ui.serve_background("127.0.0.1:0").await.unwrap();
    if scenario == "waiting" {
        seed_waiting(&host, dir.path()).await;
    }
    if scenario == "interrupted" {
        seed_interrupted(&host, dir.path()).await;
    }
    if matches!(
        scenario,
        "project-terminal" | "workbench" | "skills" | "advanced-files" | "file-viewer-consumer"
    ) {
        let workspace = dir.path().join("terminal-project");
        std::fs::create_dir_all(&workspace).unwrap();
        if scenario == "advanced-files" || scenario == "file-viewer-consumer" {
            std::fs::write(
                workspace.join("sites.geojson"),
                r#"{"type":"FeatureCollection","features":[{"type":"Feature","id":"site","properties":{"name":"Site one"},"geometry":{"type":"Point","coordinates":[1,2]}}]}"#,
            )
            .unwrap();
            std::fs::write(workspace.join("bad.geojson"), "{broken").unwrap();
            std::fs::write(workspace.join("bad.ifc"), "invalid IFC source").unwrap();
            // Above the old 25 MiB version limit, with a valid binary STL header.
            let mut bytes = vec![0u8; 84 + 600_000 * 50];
            bytes[80..84].copy_from_slice(&600_000u32.to_le_bytes());
            std::fs::write(workspace.join("large.stl"), bytes).unwrap();
            let mut small = vec![0u8; 134];
            small[80..84].copy_from_slice(&1u32.to_le_bytes());
            for (i, coordinate) in [0f32, 0., 0., 1., 0., 0., 0., 1., 0.]
                .into_iter()
                .enumerate()
            {
                small[96 + i * 4..100 + i * 4].copy_from_slice(&coordinate.to_le_bytes());
            }
            std::fs::write(workspace.join("part.stl"), small).unwrap();
        }
        let summary = host.call("summary", json!({})).await.unwrap();
        host.call(
            "project_folder",
            json!({ "id": summary["project"]["id"], "path": workspace }),
        )
        .await
        .unwrap();
    }
    let out = tokio::process::Command::new("node")
        .arg("page.mjs")
        .arg(scenario)
        .arg(&served.url)
        // A small MCP server for the `plugins` scenario.
        .env(
            "ARK_TEST_MCP",
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../ark-tools/tests/fixtures/mcp_echo.py"),
        )
        .current_dir(web_dir())
        .output()
        .await
        .expect("node is on PATH");
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{scenario} failed:\n{text}");
    if scenario == "computer" {
        let log = screen.log.lock().unwrap().clone();
        assert!(
            !log.iter().any(|l| l.starts_with("press")),
            "taken back before the click: {log:?}"
        );
    }
    if scenario == "computer-picture" {
        // The fake picture is half the window's size: pixel (100, 150) is point (200, 300).
        let log = screen.log.lock().unwrap().clone();
        assert!(
            log.contains(&"pointer Click at 200,300".to_string()),
            "{log:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn advanced_file_viewers_use_snapshots_and_keep_source_recovery() {
    drive("advanced-files", vec![], Duration::ZERO).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_consumer_file_viewer_reads_real_snapshots_in_its_frame() {
    drive("file-viewer-consumer", vec![], Duration::ZERO).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn closing_a_setup_sheet_while_it_opens_closes_the_late_terminal() {
    drive("terminal-close-race", vec![], Duration::ZERO).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_apps_conversation_draws_exactly_as_the_chat() {
    let script = vec![
        say("", true),
        say(
            "## Notes\n\nThey mention **a locket** and `the key`:\n\n```rust\nfn main() {}\n```\n\n> Next: the door.",
            false,
        ),
    ];
    drive("chat-view", script, Duration::from_millis(5)).await;
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
async fn an_unconfirmed_effect_with_trailing_whitespace_withdraws_false_success() {
    let script = vec![
        Completion {
            content: String::new(),
            tool_calls: vec![ToolCall {
                id: "send-1".into(),
                name: "message.send".into(),
                arguments: json!({}),
            }],
            usage: Usage::default(),
        },
        say("The message was sent.\n", false),
    ];
    drive("guarded-effect", script, Duration::from_millis(40)).await;
}

/// A model whose first reply breaks off after some text (an overloaded service mid-stream);
/// every later call is `inner`'s.
struct CutOff {
    inner: Arc<dyn ark_model::Provider>,
    cut: std::sync::atomic::AtomicBool,
}

#[async_trait::async_trait]
impl ark_model::Provider for CutOff {
    async fn complete(
        &self,
        system: &str,
        messages: &[ark_model::Message],
        tools: &[ark_model::ToolDef],
    ) -> ark_model::Result<Completion> {
        self.inner.complete(system, messages, tools).await
    }
    async fn complete_streaming(
        &self,
        system: &str,
        messages: &[ark_model::Message],
        tools: &[ark_model::ToolDef],
        on_text: ark_model::TextSink<'_>,
    ) -> ark_model::Result<Completion> {
        if !self.cut.swap(true, std::sync::atomic::Ordering::SeqCst) {
            for piece in ["Reading the notes, ", "then I wi"] {
                on_text(piece);
                tokio::time::sleep(Duration::from_millis(80)).await;
            }
            return Err(ark_model::ProviderError::Overloaded {
                provider: "m".into(),
                detail: "overloaded mid-stream".into(),
            });
        }
        self.inner
            .complete_streaming(system, messages, tools, on_text)
            .await
    }
    fn describe(&self) -> String {
        self.inner.describe()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn text_of_a_failed_attempt_is_taken_back_and_the_retry_shows_once() {
    let script = vec![
        say("Reading the notes now.", true),
        say("The notes mention a locket.", false),
    ];
    drive("withdrawn", script, Duration::from_millis(40)).await;
}

/// A model whose first `left` replies cannot be used (arguments that do not parse, after some
/// text), as Codex's long edit was; then `inner`'s.
struct Unusable {
    inner: Arc<dyn ark_model::Provider>,
    left: std::sync::atomic::AtomicUsize,
}

#[async_trait::async_trait]
impl ark_model::Provider for Unusable {
    async fn complete(
        &self,
        system: &str,
        messages: &[ark_model::Message],
        tools: &[ark_model::ToolDef],
    ) -> ark_model::Result<Completion> {
        self.inner.complete(system, messages, tools).await
    }
    async fn complete_streaming(
        &self,
        system: &str,
        messages: &[ark_model::Message],
        tools: &[ark_model::ToolDef],
        on_text: ark_model::TextSink<'_>,
    ) -> ark_model::Result<Completion> {
        use std::sync::atomic::Ordering::SeqCst;
        if self.left.load(SeqCst) > 0 {
            self.left.fetch_sub(1, SeqCst);
            on_text("Writing the edit now.");
            tokio::time::sleep(Duration::from_millis(60)).await;
            return Err(ark_model::ProviderError::Malformed {
                provider: "m".into(),
                detail: "arguments for notes.read are not JSON: expected `,` or `}` at line 1 column 1573".into(),
            });
        }
        self.inner
            .complete_streaming(system, messages, tools, on_text)
            .await
    }
    fn describe(&self) -> String {
        self.inner.describe()
    }
}

/// A reply the model could not use is redone quietly; one it never gets right ends the turn in
/// the person's words, with Continue (ADR 0050).
#[tokio::test(flavor = "multi_thread")]
async fn a_reply_the_model_redoes_shows_once_and_one_it_cannot_fix_says_so_plainly() {
    drive(
        "repair",
        vec![say("The notes mention a locket.", false)],
        Duration::from_millis(20),
    )
    .await;
    drive("unusable", vec![], Duration::ZERO).await;
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
async fn an_interrupted_turn_shows_what_it_did_and_continues_on_one_click() {
    drive(
        "interrupted",
        vec![say(
            "Picked up where it stopped: the notes mention a locket.",
            false,
        )],
        Duration::ZERO,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn on_windows_shortcuts_read_and_work_with_ctrl() {
    drive("windows", vec![], Duration::ZERO).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_plus_menu_opens_files_and_a_terminal_in_the_project_folder() {
    drive("workbench", vec![], Duration::ZERO).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_browser_view_follows_the_panel_and_steps_aside_for_menus() {
    drive("browserview", vec![], Duration::ZERO).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_address_that_does_not_answer_says_so_and_can_be_tried_again() {
    drive("browserdown", vec![], Duration::ZERO).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_page_that_refuses_frames_says_so_in_a_browser() {
    drive("framerefused", vec![], Duration::ZERO).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_window_on_a_runtime_elsewhere_has_its_controls_and_a_way_back() {
    drive("visit", vec![], Duration::ZERO).await;
}

fn call(name: &str, args: Value) -> Completion {
    Completion {
        content: String::new(),
        tool_calls: vec![ToolCall {
            id: "c1".into(),
            name: name.into(),
            arguments: args,
        }],
        usage: Usage::default(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn files_the_agent_writes_open_and_the_person_edits_them() {
    let script = vec![
        call(
            "files.write",
            json!({ "path": "sales.xlsx", "rows": [["Region", "Q1"], ["North", 120], ["South", 80], ["Total", "=SUM(B2:B3)"]] }),
        ),
        say("The workbook is ready.", false),
        call(
            "files.write",
            json!({ "path": "report.docx", "text": "# Sales\n\nNorth led.\n\n- Check the south" }),
        ),
        say("The report is ready.", false),
    ];
    drive("files", script, Duration::ZERO).await;
}

/// A shell as a runtime, so the page test runs a real contained program without Python.
struct Sh;

#[async_trait::async_trait]
impl ark_sdk::prelude::ark_exec::Runtime for Sh {
    fn name(&self) -> &'static str {
        "sh"
    }
    fn says(&self) -> String {
        "a POSIX shell".into()
    }
    fn available(&self) -> Result<(), String> {
        Ok(())
    }
    async fn ready(&self) -> Result<ark_sdk::prelude::ark_exec::Launch, String> {
        Ok(ark_sdk::prelude::ark_exec::Launch {
            program: "/bin/sh".into(),
            args: vec![],
            file: "main.sh",
            env: vec![],
            reads: vec![],
            home: vec![],
        })
    }
}

/// A one-page PDF with a line of text, written by hand (ASCII, so a shell can write it).
fn tiny_pdf() -> String {
    let stream = "BT /F1 12 Tf 72 720 Td (Sales grew) Tj ET";
    let objs = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [4 0 R] /Count 1 >>".to_string(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 3 0 R >> >> /Contents 5 0 R >>".to_string(),
        format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()),
    ];
    let mut out = String::from("%PDF-1.4\n");
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.push_str(&format!("{} 0 obj\n{o}\nendobj\n", i + 1));
    }
    let xref = out.len();
    out.push_str(&format!(
        "xref\n0 {}\n0000000000 65535 f \n",
        objs.len() + 1
    ));
    for o in offsets {
        out.push_str(&format!("{o:010} 00000 n \n"));
    }
    out.push_str(&format!(
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
        objs.len() + 1
    ));
    out
}

/// ADR 0035: files dropped on the conversation land in the project's folder.
#[tokio::test(flavor = "multi_thread")]
async fn files_dropped_on_the_chat_go_into_the_project() {
    drive("drop", vec![], Duration::ZERO).await;
}

/// ADR 0034: a run changes a file and removes another; the turn's cards put each back.
#[tokio::test(flavor = "multi_thread")]
async fn a_file_a_run_changed_or_removed_is_put_back_from_its_card() {
    let run = |code: &str| {
        call(
            ark_sdk::prelude::ark_exec::RUN,
            json!({ "code": code, "runtime": "sh", "purpose": "work on the notes" }),
        )
    };
    let script = vec![
        run("echo one > notes.md && echo old > old.md"),
        say("The notes are written.", false),
        run("echo two > notes.md && rm old.md"),
        say("The notes are rewritten.", false),
    ];
    drive("restore", script, Duration::ZERO).await;
}

/// "Analyze the sales": one contained run writes a chart, a table (longer than the grid shows),
/// a report that shows the chart, and a PDF; each opens beside the conversation in the view for
/// its kind.
#[tokio::test(flavor = "multi_thread")]
async fn a_run_writes_a_chart_and_a_report_that_open_beside_the_conversation() {
    let program = format!(
        "{{ printf 'region,q1\\nNorth,120\\nSouth,80\\n'; seq 1 6000 | sed 's/^/r,/'; }} > table.csv\n\
         cat > chart.svg <<'EOF'\n<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"120\" height=\"80\"><rect x=\"10\" y=\"20\" width=\"40\" height=\"60\" fill=\"#3b6fd8\"/><rect x=\"70\" y=\"40\" width=\"40\" height=\"40\" fill=\"#3b6fd8\"/></svg>\nEOF\n\
         cat > report.md <<'EOF'\n# Sales\n\nNorth led with 120.\n\n![Sales by region](chart.svg)\nEOF\n\
         cat > summary.pdf <<'EOF'\n{}EOF\n\
         echo 200",
        tiny_pdf()
    );
    let script = vec![
        call(
            ark_sdk::prelude::ark_exec::RUN,
            json!({ "code": program, "runtime": "sh", "purpose": "chart the sales and write a report" }),
        ),
        say("The chart, the table and the report are ready.", false),
    ];
    drive("analysis", script, Duration::ZERO).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unreadable_image_keeps_its_named_file_card() {
    let program = "cat > generated-image.png <<'EOF'\n<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"2\" height=\"2\"><rect width=\"2\" height=\"2\"/></svg>\nEOF";
    let script = vec![
        call(
            ark_sdk::prelude::ark_exec::RUN,
            json!({ "code": program, "runtime": "sh", "purpose": "make an image" }),
        ),
        say("The image is ready.", false),
    ];
    drive("image-failure", script, Duration::ZERO).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn settings_sections_show_and_compose_fills_the_composer_unsent() {
    drive("settings", vec![], Duration::ZERO).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn chat_search_keeps_the_composer_and_keyboard_focus() {
    drive("search", vec![say("Search answer", false)], Duration::ZERO).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn dictation_preserves_the_editable_draft_and_explains_permission_failure() {
    drive("dictation", vec![], Duration::ZERO).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn chat_switches_and_split_promotion_keep_each_draft_separate() {
    drive("draft-isolation", vec![], Duration::ZERO).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn devices_startup_skips_native_session_fetch_and_handles_bad_browser_json() {
    drive("devices-startup", vec![], Duration::ZERO).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn required_extension_failure_blocks_the_workspace_and_names_the_module() {
    drive("startup-extension-failure", vec![], Duration::ZERO).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn required_extension_may_register_after_the_shared_page() {
    drive("late-extension", vec![], Duration::ZERO).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_app_model_picker_asks_for_a_model_and_on_submit_takes_messages() {
    drive(
        "picker",
        vec![say("The notes list a locket.", false)],
        Duration::ZERO,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_app_waits_for_a_resource_location_before_its_first_project_call() {
    drive_app("ready", vec![], Duration::ZERO, true).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tool_servers_are_added_and_removed_in_settings() {
    drive("plugins", vec![], Duration::ZERO).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn marketplace_adds_an_inert_plugin_and_explains_the_connection_step() {
    drive("marketplace", vec![], Duration::ZERO).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn capabilities_connect_through_the_browser_and_disconnect() {
    drive("capabilities", vec![], Duration::ZERO).await;
}

/// An OAuth provider on loopback: its sign-in page sends the browser straight back with a
/// code, its token endpoint answers for `ada@example.com` with Mail's scope.
async fn fake_provider() -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            tokio::spawn(async move {
                // Read the whole request (head and body) before answering: closing a socket
                // with unread data resets it, and the client may lose the reply.
                let mut buf = vec![0u8; 16384];
                let mut n = 0;
                loop {
                    let k = sock.read(&mut buf[n..]).await.unwrap_or(0);
                    n += k;
                    let text = String::from_utf8_lossy(&buf[..n]).to_string();
                    let done = text.find("\r\n\r\n").is_some_and(|end| {
                        let len = text[..end]
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                            })
                            .unwrap_or(0);
                        n >= end + 4 + len
                    });
                    if done || k == 0 {
                        break;
                    }
                }
                let head = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
                let reply = if path.starts_with("/auth") {
                    let query = reqwest_url(&path);
                    let get = |k: &str| {
                        url::form_urlencoded::parse(query.as_bytes())
                            .find_map(|(name, value)| (name == k).then(|| value.into_owned()))
                            .unwrap_or_default()
                    };
                    format!(
                        "HTTP/1.1 302 Found\r\nlocation: {}?code=c0de&state={}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                        get("redirect_uri"),
                        get("state")
                    )
                } else {
                    let body = if path.starts_with("/token") {
                        json!({
                            "access_token": "a", "refresh_token": "r", "expires_in": 3600,
                            "scope": "openid email https://www.googleapis.com/auth/gmail.send",
                            "id_token": "e30.eyJlbWFpbCI6ImFkYUBleGFtcGxlLmNvbSJ9.sig"
                        })
                    } else {
                        json!({})
                    }
                    .to_string();
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    )
                };
                let _ = sock.write_all(reply.as_bytes()).await;
            });
        }
    });
    base
}

/// GET `url` and the place it redirects to, as a browser would (plain HTTP on loopback).
fn follow(url: &str) -> std::io::Result<()> {
    use std::io::{Read, Write};
    let mut url = url.to_string();
    for _ in 0..3 {
        let rest = url.trim_start_matches("http://");
        let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
        // The browser callback runs on a separate thread while the loopback provider and ARK
        // callback listener are starting. A busy full-suite run can reach either address before
        // its one-shot route is ready, or see the close as a reset after the complete response.
        // Retry the navigation hop instead of silently dropping the sign-in.
        let mut next = None;
        for _ in 0..50 {
            let Ok(mut stream) = std::net::TcpStream::connect(host) else {
                std::thread::sleep(Duration::from_millis(20));
                continue;
            };
            if write!(
                stream,
                "GET /{path} HTTP/1.1\r\nhost: {host}\r\nconnection: close\r\n\r\n"
            )
            .is_err()
            {
                std::thread::sleep(Duration::from_millis(20));
                continue;
            }
            let mut reply = String::new();
            if let Err(error) = stream.read_to_string(&mut reply) {
                let closed_after_reply = !reply.is_empty()
                    && matches!(
                        error.kind(),
                        std::io::ErrorKind::ConnectionReset
                            | std::io::ErrorKind::ConnectionAborted
                            | std::io::ErrorKind::BrokenPipe
                    );
                if !closed_after_reply {
                    std::thread::sleep(Duration::from_millis(20));
                    continue;
                }
            }
            if let Some(location) = reply.lines().find_map(|l| l.strip_prefix("location: ")) {
                next = Some(location.trim().to_string());
                break;
            }
            if reply.starts_with("HTTP/1.1 2") {
                return Ok(());
            }
            if reply.starts_with("HTTP/1.1 404") {
                std::thread::sleep(Duration::from_millis(20));
                continue;
            }
            return Err(std::io::Error::other(format!(
                "unexpected response from {host} for {url}: {}",
                reply.lines().next().unwrap_or("empty response")
            )));
        }
        let Some(next) = next else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("could not complete {url}"),
            ));
        };
        url = next;
    }
    Ok(())
}

/// The query of a request path.
fn reqwest_url(path: &str) -> String {
    path.split_once('?')
        .map(|(_, q)| q.to_string())
        .unwrap_or_default()
}

/// The agent needs Mail mid-task: the card says what for and offers Connect; after the
/// sign-in the same turn sends, which the person approves as usual.
#[tokio::test(flavor = "multi_thread")]
async fn an_agent_asks_to_connect_mail_and_the_turn_goes_on() {
    let acquire = Completion {
        content: String::new(),
        tool_calls: vec![ToolCall {
            id: "c1".into(),
            name: "acquire_capability".into(),
            arguments: json!({ "need": "mail" }),
        }],
        usage: Usage::measured(50, 5),
    };
    let send = Completion {
        content: String::new(),
        tool_calls: vec![ToolCall {
            id: "c2".into(),
            name: "mail.send".into(),
            arguments: json!({ "to": ["bob@example.com"], "subject": "Report", "body": "Ready." }),
        }],
        usage: Usage::measured(50, 5),
    };
    let script = vec![acquire, send, say("Sent the report to Bob.", false)];
    drive("connect-mid-task", script, Duration::ZERO).await;
}

/// "Always in this project": the next turn's save runs without a card, Settings › Permissions
/// lists the grant, and removing it brings the question back.
#[tokio::test(flavor = "multi_thread")]
async fn always_in_this_project_stops_the_question_until_removed() {
    let save = |id: &str| Completion {
        content: String::new(),
        tool_calls: vec![ToolCall {
            id: id.into(),
            name: "notes.save".into(),
            arguments: json!({}),
        }],
        usage: Usage::default(),
    };
    let script = vec![
        save("c1"),
        say("Saved.", false),
        save("c2"),
        say("Saved again.", false),
        save("c3"),
        say("Saved a third time.", false),
    ];
    drive("grants", script, Duration::ZERO).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_apps_action_on_a_project_is_in_its_menu() {
    drive("project-action", vec![], Duration::ZERO).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_project_row_starts_a_chat_in_that_project() {
    drive("project-new-chat", vec![], Duration::ZERO).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn every_project_can_stay_expanded() {
    drive("expanded-projects", vec![], Duration::ZERO).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_project_can_be_removed_from_the_sidebar_without_deleting_it() {
    drive("hidden-project", vec![], Duration::ZERO).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn chat_tabs_composer_and_split_share_one_visual_model() {
    drive("chat-workspace-polish", vec![], Duration::ZERO).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn chat_tabs_form_nested_resizable_window_layouts() {
    drive("spatial-chat-layouts", vec![], Duration::ZERO).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_detached_chat_window_keeps_its_own_foreground_and_layout() {
    drive("detached-chat-window", vec![], Duration::ZERO).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn skills_are_listed_in_settings_and_added_from_the_terminal() {
    drive("skills", vec![], Duration::ZERO).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_narrow_window_keeps_the_sidebar_reachable() {
    drive("narrow", vec![], Duration::ZERO).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_narrow_window_shows_what_opens_in_the_side_pane_and_comes_back() {
    drive("narrow-pane", vec![], Duration::ZERO).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn primary_buttons_read_in_light_and_dark_with_any_accent() {
    let script = vec![call("notes.save", json!({})), say("Saved.", false)];
    drive("accent", script, Duration::ZERO).await;
}

/// ADR 0045: the Computer row in Plugins turns it on and off, and an agent that needs it while
/// it is off asks "Allow ARK to use the apps on this computer"; Allow turns it on and the turn
/// goes on in the app.
#[tokio::test(flavor = "multi_thread")]
async fn the_computer_turns_on_and_off_in_plugins_and_an_agent_asks_to_use_the_apps() {
    let script = vec![
        call(
            "acquire_capability",
            json!({ "need": "use the apps on this computer" }),
        ),
        call("computer.apps", json!({})),
        say("Notes is open.", false),
    ];
    drive("computer-switch", script, Duration::ZERO).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_project_terminal_opens_without_changing_the_chat() {
    drive("project-terminal", vec![], Duration::ZERO).await;
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

/// What waits for the person (ADR 0043): a chat named "hi" with a result posted into it while
/// the person was in another chat, and three failures with no chat.
/// A chat whose turn a crashed process left begun (ADR 0048): it read the notes, then the
/// process died. The next process to open the store marks it interrupted.
async fn seed_interrupted(host: &Arc<ark_ui::Host>, dir: &std::path::Path) {
    let s = host.call("summary", json!({})).await.unwrap();
    let project = ProjectId(s["project"]["id"].as_str().unwrap().to_string());
    let ark = Ark::builder().store_root(dir).build().unwrap();
    let mut chat = ark.start_session(&project).unwrap();
    chat.set_title(ark.engine(), "Read the notes, then summarize them")
        .unwrap();
    let id = chat.id().as_str().to_string();
    let store = &ark.engine().store;
    store
        .put_session_turn(
            &id,
            0,
            &json!({
                "index": 0, "input": "Read the notes, then summarize them", "answer": "",
                "cancelled": false, "steered": [], "at": "2026-10-07T02:00:00Z",
                "incomplete": true, "pid": u32::MAX - 1, "pid_start": 1
            }),
        )
        .unwrap();
    let mut read =
        ark_sdk::prelude::ark_core::Invocation::new("notes.read", RiskTier::Read, json!({}));
    read.work = Some(chat.work_of(0));
    read.mark_ok(json!({ "text": "remember the locket" }));
    store.put_invocation(&project, &read).unwrap();
    drop(chat);
    drop(ark);
    // The next process opens the store.
    Ark::builder().store_root(dir).build().unwrap();
}

async fn seed_waiting(host: &Arc<ark_ui::Host>, dir: &std::path::Path) {
    let mut rx = host.subscribe();
    host.call("send", json!({ "text": "hi" })).await.unwrap();
    loop {
        let v = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("the turn ends")
            .unwrap();
        if v["kind"] == "turn_finished" {
            break;
        }
    }
    let s = host.call("summary", json!({})).await.unwrap();
    let (project, chat) = (
        s["project"]["id"].as_str().unwrap().to_string(),
        s["session"]["id"].as_str().unwrap().to_string(),
    );
    host.call("new_session", json!({})).await.unwrap();
    let ark = Ark::builder().store_root(dir).build().unwrap();
    let store = &ark.engine().store;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let task = |id: &str, objective: &str, state: &str, at: u64, chat: Option<&str>| {
        let mut t = json!({
            "id": id, "project": project, "owner": "scribe", "objective": objective,
            "origin": { "by": "external" }, "state": state, "created_ms": at - 1000,
            "finished_ms": at, "error": "The notes folder is gone.", "result": "Filed.",
        });
        if let Some(c) = chat {
            t["report_to"] = json!(c);
        }
        store.put_record("orch_task", id, &project, &t).unwrap();
    };
    task("t1", "Summarize the notes", "failed", now - 40_000, None);
    task("t2", "Tidy the drafts", "failed", now - 30_000, None);
    task("t3", "Rename the chapters", "failed", now - 20_000, None);
    task("t4", "File the notes", "done", now - 10_000, Some(&chat));
    ark_sdk::prelude::ark_runtime::session::post(
        ark.engine(),
        &chat,
        ark_sdk::prelude::ark_runtime::Post::new("scribe", "File the notes", "Filed.", true)
            .work("t4"),
    )
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn work_that_waits_for_the_person_sits_in_the_tray() {
    drive_app("waiting", vec![say("Hello.", false)], Duration::ZERO, true).await;
}

/// The board: add, drag, complete, open an item, hand it to an agent and see it move by itself,
/// what needs the person, delete with undo.
#[tokio::test(flavor = "multi_thread")]
async fn the_board_is_worked_by_the_person_and_the_agents() {
    drive_app(
        "board",
        vec![say("Chapter 3 is drafted.", false)],
        Duration::ZERO,
        true,
    )
    .await;
}

/// Schedules (ADR 0053): made in words and approved each time, shown as a line above the
/// composer, run now, paused, resumed and stopped from beside the chat.
#[tokio::test(flavor = "multi_thread")]
async fn a_schedule_asked_in_words_is_seen_run_paused_and_stopped_beside_its_chat() {
    let schedule = Completion {
        content: String::new(),
        tool_calls: vec![ToolCall {
            id: "c1".into(),
            name: "agents.schedule".into(),
            arguments: json!({
                "objective": "Recount the stock in stock.xlsx and summarize what changed",
                "at": "02:00"
            }),
        }],
        usage: Usage::default(),
    };
    let script = vec![
        schedule,
        say("Every day at 02:00 I will recount the stock.", false),
        // The run the person starts with Run now.
        say("Counted 120 boxes; 3 are missing since yesterday.", false),
    ];
    drive_app("schedules", script, Duration::ZERO, true).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn schedule_destinations_cover_states_recovery_and_project_races() {
    drive_app("schedule-destination-states", vec![], Duration::ZERO, true).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_chat_keeps_working_in_the_background_while_another_runs() {
    let save = Completion {
        content: String::new(),
        tool_calls: vec![ToolCall {
            id: "c1".into(),
            name: "notes.save".into(),
            arguments: json!({}),
        }],
        usage: Usage::default(),
    };
    // In call order: the first chat asks to save and waits; the second answers; then the
    // first finishes once allowed.
    let script = vec![
        save,
        say("Second answer.", false),
        say("Saved the notes.", false),
    ];
    drive("background", script, Duration::ZERO).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_person_sees_who_uses_the_computer_and_takes_it_back() {
    let call = |name: &str, args: Value| Completion {
        content: String::new(),
        tool_calls: vec![ToolCall {
            id: name.into(),
            name: name.into(),
            arguments: args,
        }],
        usage: Usage::default(),
    };
    let script = vec![
        call("computer.look", json!({ "app": "Notes" })),
        call(
            "computer.click",
            json!({ "app": "Notes", "ref": 2, "name": "Save" }),
        ),
        say(
            "You took the computer back, so I did not save the note.",
            false,
        ),
    ];
    drive("computer", script, Duration::ZERO).await;
}

/// A picture the agent took shows under its step, and a click in it is asked with the picture and
/// a ring on the spot (ADR 0042).
#[tokio::test(flavor = "multi_thread")]
async fn the_person_sees_what_the_agent_saw_and_where_it_will_click() {
    let call = |name: &str, args: Value| Completion {
        content: String::new(),
        tool_calls: vec![ToolCall {
            id: name.into(),
            name: name.into(),
            arguments: args,
        }],
        usage: Usage::default(),
    };
    let script = vec![
        call("computer.see", json!({ "app": "Notes" })),
        call(
            "computer.point",
            json!({ "app": "Notes", "x": 100, "y": 150, "what": "the Save button" }),
        ),
        say("Clicked Save.", false),
    ];
    drive("computer-picture", script, Duration::ZERO).await;
}

/// "Run the app and show me /home": the start asks with its command and folder, the Processes
/// panel shows it with its output, the page opens in the Browser once its address answers, and
/// Stop ends it (ADR 0041).
#[tokio::test(flavor = "multi_thread")]
async fn a_process_shows_its_output_and_opens_its_page() {
    if std::process::Command::new("python3")
        .arg("-V")
        .output()
        .is_err()
    {
        eprintln!("skipped: no python3");
        return;
    }
    let script = vec![
        call(
            "process.start",
            json!({ "command": "python3 -u -m http.server 0 --bind 127.0.0.1", "open": "/home", "wait": 20 }),
        ),
        say(
            "The app runs; its home page is open beside the chat.",
            false,
        ),
    ];
    drive("processes", script, Duration::ZERO).await;
}
