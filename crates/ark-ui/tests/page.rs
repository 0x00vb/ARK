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
    if scenario == "skills" {
        let skill = dir.path().join("skills/release-notes");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            "---\nname: release-notes\ndescription: Write release notes from a list of changes\n---\nSteps.\n",
        )
        .unwrap();
    }
    let model: Arc<dyn ark_model::Provider> =
        Arc::new(MockProvider::new("m", script).with_chunk_delay(chunk));
    let mut builder = Ark::builder()
        .store_root(dir.path())
        .tool(Arc::new(Notes))
        .profile(AgentProfile::builder().core_actions(["notes.read"]).build())
        .provider(model)
        .planner(false)
        .critic(false);
    if scenario == "capabilities" || scenario == "connect-mid-task" {
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
    if scenario == "computer" {
        builder = builder.computer_with(
            ark_sdk::prelude::ark_computer::ComputerService::with_screen(Arc::new(screen.clone())),
        );
    }
    if scenario == "files" {
        builder = builder.files(true);
    }
    if scenario == "analysis" {
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
            std::thread::spawn(move || follow(&url));
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
    let (_host, served) = ArkUi::new(app)
        .serve_background("127.0.0.1:0")
        .await
        .unwrap();
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
async fn settings_sections_show_and_compose_fills_the_composer_unsent() {
    drive("settings", vec![], Duration::ZERO).await;
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
async fn tool_servers_are_added_and_removed_in_settings() {
    drive("plugins", vec![], Duration::ZERO).await;
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
                    let url = reqwest_url(&path);
                    let get = |k: &str| {
                        url.split('&')
                            .find_map(|kv| kv.strip_prefix(&format!("{k}=")))
                            .map(|v| v.replace("%3A", ":").replace("%2F", "/"))
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
fn follow(url: &str) {
    use std::io::{Read, Write};
    let mut url = url.to_string();
    for _ in 0..3 {
        let rest = url.trim_start_matches("http://");
        let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
        let Ok(mut s) = std::net::TcpStream::connect(host) else {
            return;
        };
        let _ = write!(
            s,
            "GET /{path} HTTP/1.1\r\nhost: {host}\r\nconnection: close\r\n\r\n"
        );
        let mut reply = String::new();
        let _ = s.read_to_string(&mut reply);
        match reply.lines().find_map(|l| l.strip_prefix("location: ")) {
            Some(next) => url = next.trim().to_string(),
            None => return,
        }
    }
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
async fn skills_are_listed_in_settings_and_added_from_the_terminal() {
    drive("skills", vec![], Duration::ZERO).await;
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
