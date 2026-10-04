//! The interaction layer end to end with a mock model: a turn runs, its trace is stored,
//! the transcript restores activity, commands return reports, sessions and projects switch.

use std::sync::Arc;

use ark_interact::{ActivityKind, App, ChannelApprover, Effect, Interaction, Start, TurnControl};
use ark_sdk::prelude::ark_model::{Completion, MockProvider, ToolCall, Usage};
use ark_sdk::prelude::*;

struct Notes;
#[async_trait::async_trait]
impl Adapter for Notes {
    fn name(&self) -> &str {
        "notes"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![ActionDescriptor::new(
            "notes.read",
            "read the notes",
            RiskTier::Read,
        )]
    }
    async fn run(
        &self,
        _a: &str,
        _i: serde_json::Value,
        _c: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        Ok(ActionOutput::json(
            serde_json::json!({ "text": "remember the locket" }),
        ))
    }
}

fn open(dir: &std::path::Path) -> Interaction {
    let mock = MockProvider::new("m", vec![]).with_fallback(|req| {
        if req.messages.iter().any(|m| m.tool_call_id.is_some()) {
            Completion {
                content: "The notes mention a locket [inv:1].".into(),
                tool_calls: vec![],
                usage: Usage::measured(200, 10).with_cached(120),
            }
        } else {
            let call = ToolCall {
                id: "c1".into(),
                name: "notes.read".into(),
                arguments: serde_json::json!({}),
            };
            Completion {
                content: String::new(),
                tool_calls: vec![call],
                usage: Usage::measured(150, 5),
            }
        }
    });
    let provider: Arc<dyn ark_model::Provider> = Arc::new(mock);
    let ark = Ark::builder()
        .store_root(dir)
        .tool(Arc::new(Notes))
        .profile(AgentProfile::builder().core_actions(["notes.read"]).build())
        .provider(provider)
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let (approver, _rx) = ChannelApprover::channel();
    App::new("Test")
        .ark(ark)
        .labels(
            ark_interact::ActivityLabels::new().label("notes.read", |_| "Reading the notes".into()),
        )
        .open(&Start::default(), Arc::new(approver))
        .unwrap()
        .interaction
}

#[tokio::test]
async fn a_turn_is_described_traced_stored_and_restored() {
    let dir = tempfile::tempdir().unwrap();
    let mut ix = open(dir.path());
    let ctl = TurnControl::new();
    let mut labels = Vec::new();
    let end = ix
        .run_turn("what do the notes say", &ctl, |_, a| {
            if let Some(a) = a {
                labels.push((a.kind, a.label.clone()));
            }
        })
        .await;
    let out = end.result.unwrap();
    assert!(out.answer.contains("locket"));
    assert_eq!(end.index, Some(0));
    assert!(end.footer.iter().any(|f| f == "1 tool"), "{:?}", end.footer);
    assert!(labels.contains(&(ActivityKind::Tool, "Reading the notes".into())));

    // The trace is interpreted from events, with the call's context contribution.
    let trace = ix.trace(None).unwrap();
    assert_eq!(trace.calls.len(), 1);
    assert!(trace.calls[0].context_tokens.unwrap() > 0);
    assert_eq!(trace.calls[0].seen_by_requests, 1);
    assert_eq!(trace.cached_tokens, 120);

    // Reopened in a new interaction, the transcript restores the same activity from the store.
    let sid = ix.session().id().as_str().to_string();
    drop(ix);
    let mut again = open(dir.path());
    assert!(again.resume(&sid).unwrap());
    let turns = again.transcript(None, 10).unwrap();
    assert_eq!(turns.len(), 1);
    assert!(turns[0].has_trace);
    assert!(turns[0]
        .activity
        .iter()
        .any(|a| a.label == "Reading the notes"));
    assert!(
        again.trace(Some(0)).is_some(),
        "an old turn's trace is read back from the store"
    );
}

#[tokio::test]
async fn commands_return_structured_reports_and_effects() {
    let dir = tempfile::tempdir().unwrap();
    let mut ix = open(dir.path());
    let ctl = TurnControl::new();
    ix.run_turn("read the notes", &ctl, |_, _| {})
        .await
        .result
        .unwrap();

    let ctx = ix.execute("/context").await;
    assert_eq!(ctx.report.data.as_ref().unwrap().view, "context");
    assert!(ctx.report.plain_text().contains("Kept out of the request"));

    let usage = ix.execute("/usage").await.report;
    assert!(
        usage.plain_text().contains("cached input"),
        "{}",
        usage.plain_text()
    );

    let trace = ix.execute("/trace 1").await.report;
    assert!(
        trace.plain_text().contains("#1 notes.read"),
        "{}",
        trace.plain_text()
    );

    assert_eq!(ix.execute("/clear").await.effect, Some(Effect::ClearView));
    assert_eq!(
        ix.execute("hello").await.effect,
        Some(Effect::Submit("hello".into()))
    );
    assert_eq!(ix.execute("  ").await.effect, None);
    assert!(ix
        .execute("/nope")
        .await
        .report
        .plain_text()
        .contains("Unknown command"));
    let help = ix.execute("/help").await.report.plain_text();
    assert!(help.contains("/context") && help.contains("/rename"));

    ix.execute("/rename Locket hunt").await;
    assert_eq!(ix.session().record().title, "Locket hunt");
    let first = ix.session().id().clone();
    ix.execute("/project other").await;
    assert_eq!(ix.project().name, "other");
    assert_ne!(ix.session().id(), &first);
    let all = ix.sessions(true).unwrap();
    assert!(all.iter().any(|s| s.title == "Locket hunt" && !s.current));
}

#[tokio::test]
async fn projects_are_folders_and_reopen_where_they_left_off() {
    let dir = tempfile::tempdir().unwrap();
    let mut ix = open(dir.path());
    let ctl = TurnControl::new();
    ix.run_turn("read the notes", &ctl, |_, _| {})
        .await
        .result
        .unwrap();
    let home = ix.project().clone();
    let chat = ix.session().id().clone();

    // A folder becomes a project named after it; adding it again opens the same project.
    let folder = tempfile::tempdir().unwrap();
    let novel = folder.path().join("novel");
    std::fs::create_dir(&novel).unwrap();
    let p = ix.add_project("", Some(&novel)).unwrap();
    assert_eq!(p.name, "novel");
    assert_ne!(
        ix.session().id(),
        &chat,
        "a project without chats starts one"
    );
    assert_eq!(ix.add_project("", Some(&novel)).unwrap().id, p.id);
    assert!(ix.add_project("", Some(&novel.join("missing"))).is_err());

    // Another folder with the same name gets a free name; renaming onto a taken one fails.
    let other = folder.path().join("b").join("novel");
    std::fs::create_dir_all(&other).unwrap();
    let second = ix.add_project("", Some(&other)).unwrap();
    assert_eq!(second.name, "novel 2");
    assert!(ix.rename_project(second.id.as_str(), "novel").is_err());
    assert_eq!(
        ix.rename_project(second.id.as_str(), "draft").unwrap().name,
        "draft"
    );
    assert_eq!(ix.project().name, "draft");

    // Opening a project returns to its last chat; its chats are listed by project id.
    ix.open_project(home.id.as_str()).unwrap();
    assert_eq!(ix.session().id(), &chat);
    let chats = ix.project_sessions(home.id.as_str()).unwrap();
    assert!(chats.iter().any(|s| s.id == chat.as_str() && s.current));
    assert!(ix.open_project("nope").is_err());
    let names: Vec<_> = ix.projects().unwrap().into_iter().map(|p| p.name).collect();
    assert!(names.contains(&"novel".into()) && names.contains(&"draft".into()));
}

#[tokio::test]
async fn late_steering_comes_back_undelivered() {
    let dir = tempfile::tempdir().unwrap();
    let mut ix = open(dir.path());
    let ctl = TurnControl::new();
    let steer = ctl.steer.clone();
    let end = ix
        .run_turn("read the notes", &ctl, |ev, _| {
            if matches!(ev, HarnessEvent::Answer { .. }) {
                steer.push("and the door?");
            }
        })
        .await;
    assert_eq!(end.undelivered, vec!["and the door?"]);
}

/// `/agents` shows the orchestrator's graph and one task's trace, from its own records.
#[tokio::test]
async fn agents_command_shows_the_graph_and_a_task() {
    use ark_interact::ark_orchestrate::{AgentSpec, Orchestrator, TaskSpec};
    let dir = tempfile::tempdir().unwrap();
    let worker: Arc<dyn ark_model::Provider> = Arc::new(MockProvider::new(
        "w",
        vec![Completion {
            content: "WORKER-RESULT".into(),
            tool_calls: vec![],
            usage: Usage::measured(50, 5),
        }],
    ));
    let ark = Ark::builder()
        .store_root(dir.path())
        .provider(worker.clone())
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.create_project("team", "").unwrap();
    let orch = Orchestrator::builder(&ark, &project)
        .configure_models(|b| b.provider("w", worker))
        .build()
        .unwrap();
    orch.spawn(AgentSpec::new("worker").model("w"))
        .await
        .unwrap();
    let done = orch
        .run("worker", TaskSpec::new("do the thing"))
        .await
        .unwrap();

    let (approver, _rx) = ChannelApprover::channel();
    let mut ix = App::new("Team")
        .ark(ark)
        .orchestrator(orch)
        .open(&Start::default(), Arc::new(approver))
        .unwrap()
        .interaction;
    assert!(ix.commands().iter().any(|c| c.name == "agents"));
    let g = ix.execute("/agents").await.report;
    let text = g.plain_text();
    assert!(text.contains("worker"), "{text}");
    assert!(text.contains(&done.id));
    assert_eq!(g.data.as_ref().unwrap().view, "orchestration");
    let t = ix.execute(&format!("/agents {}", done.id)).await.report;
    let text = t.plain_text();
    assert!(text.contains("WORKER-RESULT"), "{text}");
    assert!(text.contains("started (attempt 1)"));
    // A single-agent app does not offer it.
    let (approver, _rx) = ChannelApprover::channel();
    let dir2 = tempfile::tempdir().unwrap();
    let solo = App::new("Solo")
        .ark(
            Ark::builder()
                .store_root(dir2.path())
                .provider(Arc::new(MockProvider::new("m", vec![])))
                .build()
                .unwrap(),
        )
        .open(&Start::default(), Arc::new(approver))
        .unwrap()
        .interaction;
    assert!(!solo.commands().iter().any(|c| c.name == "agents"));
}

/// With several agents, the person asks in words: the chat creates an agent and hands it a
/// task, and the task is in the graph like any other.
#[tokio::test]
async fn the_chat_creates_agents_and_hands_them_tasks() {
    use ark_interact::ark_orchestrate::{Orchestrator, CREATE, DELEGATE};
    let dir = tempfile::tempdir().unwrap();
    let call = |name: &str, args: serde_json::Value| Completion {
        content: String::new(),
        tool_calls: vec![ToolCall {
            id: name.into(),
            name: name.into(),
            arguments: args,
        }],
        usage: Usage::measured(100, 5),
    };
    let model: Arc<dyn ark_model::Provider> =
        Arc::new(MockProvider::new("m", vec![]).with_fallback(move |req| {
            let text = |c: &str| Completion {
                content: c.into(),
                tool_calls: vec![],
                usage: Usage::measured(100, 5),
            };
            if req.system.contains("You are the agent \"reviewer\"") {
                return text("REVIEWED: two typos.");
            }
            let done: Vec<&str> = req
                .messages
                .iter()
                .filter_map(|m| m.tool_call_id.as_deref())
                .collect();
            if done.contains(&DELEGATE) {
                text("The reviewer found two typos.")
            } else if done.contains(&CREATE) {
                call(
                    DELEGATE,
                    serde_json::json!({ "to": "reviewer", "objective": "review chapter one" }),
                )
            } else {
                call(
                    CREATE,
                    serde_json::json!({ "name": "Reviewer", "role": "reviews drafts" }),
                )
            }
        }));
    let ark = Ark::builder()
        .store_root(dir.path())
        .provider(model.clone())
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let (approver, _rx) = ChannelApprover::channel();
    let mut ix = App::new("Team")
        .ark(ark)
        .orchestrate(move |ark, project, gate| {
            let m = model.clone();
            Ok(Orchestrator::builder(ark, project)
                .configure_models(|b| b.provider("m", m).default_model("m"))
                .approvals(gate)
                .build()?)
        })
        .open(&Start::default(), Arc::new(approver))
        .unwrap()
        .interaction;

    let ctl = TurnControl::new();
    let mut labels = Vec::new();
    let end = ix
        .run_turn("have someone review chapter one", &ctl, |_, a| {
            if let Some(a) = a {
                labels.push(a.label.clone());
            }
        })
        .await;
    assert!(end.result.unwrap().answer.contains("two typos"));
    assert!(
        labels.iter().any(|l| l == "Creating agent Reviewer"),
        "{labels:?}"
    );
    assert!(
        labels
            .iter()
            .any(|l| l.starts_with("Delegating to reviewer")),
        "{labels:?}"
    );
    let orch = ix.orchestrator().unwrap();
    let agent = orch.agent("reviewer").unwrap();
    assert_eq!(agent.spec.role, "reviews drafts");
    let tasks = orch.tasks();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].owner, "reviewer");
    assert!(tasks[0].result.as_deref().unwrap().contains("REVIEWED"));
}

fn echo_server() -> Option<String> {
    let ok = std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !ok {
        eprintln!("python3 not found: skipped");
        return None;
    }
    let p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../ark-tools/tests/fixtures/mcp_echo.py");
    Some(p.canonicalize().unwrap().display().to_string())
}

/// A model that calls `action` once, then answers with what the call returned.
fn caller(action: &'static str) -> Arc<dyn ark_model::Provider> {
    Arc::new(MockProvider::new("m", vec![]).with_fallback(move |req| {
        let returned: Vec<String> = req
            .messages
            .iter()
            .filter(|m| m.tool_call_id.is_some())
            .map(|m| m.content.clone())
            .collect();
        if returned.is_empty() {
            Completion {
                content: String::new(),
                tool_calls: vec![ToolCall {
                    id: "c1".into(),
                    name: action.into(),
                    arguments: serde_json::json!({ "text": "echoed back" }),
                }],
                usage: Usage::measured(100, 5),
            }
        } else {
            Completion {
                content: format!("GOT: {}", returned.join(" | ")),
                tool_calls: vec![],
                usage: Usage::measured(100, 5),
            }
        }
    }))
}

/// `/plugin add` starts an MCP server while the app runs; the next turn can call its tools,
/// and an orchestrated agent gets them only when its `tools` allow.
#[tokio::test]
async fn a_plugin_added_by_command_serves_the_next_turn_and_the_agents_it_is_given_to() {
    use ark_interact::ark_orchestrate::{AgentSpec, Orchestrator, TaskSpec};
    let Some(srv) = echo_server() else { return };
    let dir = tempfile::tempdir().unwrap();
    let model = caller("echo.echo");
    let ark = Ark::builder()
        .store_root(dir.path())
        .provider(model.clone())
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let (approver, _rx) = ChannelApprover::channel();
    let mut ix = App::new("Test")
        .ark(ark)
        .open(&Start::default(), Arc::new(approver))
        .unwrap()
        .interaction;

    let none = ix.execute("/plugin").await.report.plain_text();
    assert!(none.contains("No plugins"), "{none}");
    let added = ix
        .execute(&format!("/plugin add echo python3 \"{srv}\""))
        .await
        .report
        .plain_text();
    assert!(added.contains("Added echo (2 tools)"), "{added}");
    assert!(added.contains("echo.echo, echo.stamp"), "{added}");
    let tools = ix.execute("/tools").await.report.plain_text();
    assert!(tools.contains("echo.echo"), "{tools}");

    let ctl = TurnControl::new();
    let end = ix.run_turn("say it back", &ctl, |_, _| {}).await;
    let answer = end.result.unwrap().answer;
    assert!(answer.contains("echoed back"), "{answer}");

    // Agents: one may use the server, one may not.
    let project = ix.project().clone();
    let orch = Orchestrator::builder(ix.ark(), &project)
        .configure_models(|b| b.provider("m", model.clone()).default_model("m"))
        .build()
        .unwrap();
    orch.spawn(AgentSpec::new("allowed").tools(["echo.*"]))
        .await
        .unwrap();
    orch.spawn(AgentSpec::new("other").tools(["files.*"]))
        .await
        .unwrap();
    let ok = orch
        .run("allowed", TaskSpec::new("say it back"))
        .await
        .unwrap();
    assert!(
        ok.result.as_deref().unwrap().contains("echoed back"),
        "{ok:?}"
    );
    let no = orch
        .run("other", TaskSpec::new("say it back"))
        .await
        .unwrap();
    assert!(
        !no.result
            .as_deref()
            .unwrap_or_default()
            .contains("echoed back"),
        "an agent without the server's tools cannot call them: {no:?}"
    );

    let removed = ix.execute("/plugin remove echo").await.report.plain_text();
    assert!(
        removed.contains("Removed echo") && removed.contains("No plugins"),
        "{removed}"
    );
}

/// A skill in the store: the conversation sees its one line, reads its instructions with
/// `skills.read`, and `/skills` lists it. An agent sees it only when its spec names it.
#[tokio::test]
async fn the_conversation_reads_a_skill_and_agents_get_the_ones_they_are_given() {
    use ark_interact::ark_orchestrate::{AgentSpec, Orchestrator, TaskSpec};
    let dir = tempfile::tempdir().unwrap();
    let skill = dir.path().join("skills/release-notes");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: release-notes\ndescription: Write release notes from a list of changes\n---\nStart with a line that says NOTES-FORMAT-7.\n",
    )
    .unwrap();
    let seen: Arc<std::sync::Mutex<Vec<String>>> = Default::default();
    let log = seen.clone();
    let model: Arc<dyn ark_model::Provider> =
        Arc::new(MockProvider::new("m", vec![]).with_fallback(move |req| {
            let first = req
                .messages
                .first()
                .map(|m| m.content.clone())
                .unwrap_or_default();
            log.lock().unwrap().push(first.clone());
            let returned: Vec<String> = req
                .messages
                .iter()
                .filter(|m| m.tool_call_id.is_some())
                .map(|m| m.content.clone())
                .collect();
            if returned.is_empty() && first.contains("== SKILLS ==") {
                Completion {
                    content: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "s1".into(),
                        name: "skills.read".into(),
                        arguments: serde_json::json!({ "name": "release-notes" }),
                    }],
                    usage: Usage::measured(100, 5),
                }
            } else {
                Completion {
                    content: format!("ANSWER {}", returned.join(" | ")),
                    tool_calls: vec![],
                    usage: Usage::measured(100, 5),
                }
            }
        }));
    let ark = Ark::builder()
        .store_root(dir.path())
        .provider(model.clone())
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let (approver, _rx) = ChannelApprover::channel();
    let mut ix = App::new("Test")
        .ark(ark)
        .open(&Start::default(), Arc::new(approver))
        .unwrap()
        .interaction;

    let listed = ix.execute("/skills").await.report.plain_text();
    assert!(
        listed.contains("release-notes") && listed.contains("all projects"),
        "{listed}"
    );

    let ctl = TurnControl::new();
    let answer = ix
        .run_turn("write notes for these changes", &ctl, |_, _| {})
        .await
        .result
        .unwrap()
        .answer;
    assert!(
        answer.contains("NOTES-FORMAT-7"),
        "the instructions reached the model: {answer}"
    );
    let first = seen.lock().unwrap()[0].clone();
    assert!(
        first.contains("- release-notes: Write release notes"),
        "{first}"
    );
    assert!(
        !first.contains("NOTES-FORMAT-7"),
        "instructions are not sent up front"
    );

    // Agents: only the one given the skill sees it.
    let project = ix.project().clone();
    let orch = Orchestrator::builder(ix.ark(), &project)
        .configure_models(|b| b.provider("m", model.clone()).default_model("m"))
        .build()
        .unwrap();
    orch.spawn(AgentSpec::new("writer").skills(["release-notes"]))
        .await
        .unwrap();
    orch.spawn(AgentSpec::new("other")).await.unwrap();
    seen.lock().unwrap().clear();
    let w = orch
        .run("writer", TaskSpec::new("write the notes"))
        .await
        .unwrap();
    assert!(
        w.result.as_deref().unwrap().contains("NOTES-FORMAT-7"),
        "{w:?}"
    );
    seen.lock().unwrap().clear();
    orch.run("other", TaskSpec::new("write the notes"))
        .await
        .unwrap();
    assert!(
        seen.lock()
            .unwrap()
            .iter()
            .all(|m| !m.contains("== SKILLS ==")),
        "an agent without skills gets no catalog"
    );
}

/// `/connect` lists what ARK can use in a person's words: the web is ready, Mail and Calendar
/// say why this build cannot connect them (no sign-in client), and `/tools` lists the web.
#[tokio::test]
async fn connect_lists_capabilities_and_says_why_one_cannot_connect() {
    use ark_sdk::prelude::ark_connect::{Catalog, Connections, MemorySecrets};
    let dir = tempfile::tempdir().unwrap();
    let mut catalog = Catalog::builtin();
    for p in &mut catalog.providers {
        p.oauth.client_id.clear();
        p.oauth.client_id_env = None;
    }
    let ark = Ark::builder()
        .store_root(dir.path())
        .provider(caller("web.search"))
        .web(true)
        .connections_with(Connections::open(
            dir.path(),
            catalog,
            Arc::new(MemorySecrets::default()),
        ))
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let (approver, _rx) = ChannelApprover::channel();
    let mut ix = App::new("Test")
        .ark(ark)
        .open(&Start::default(), Arc::new(approver))
        .unwrap()
        .interaction;
    let list = ix.execute("/connect").await.report.plain_text();
    let line = |name: &str| {
        list.lines()
            .find(|l| l.contains(name))
            .unwrap_or("")
            .to_string()
    };
    assert!(line("Web").trim_start().starts_with('✓'), "{list}");
    assert!(line("Mail").trim_start().starts_with('!'), "{list}");
    assert!(list.contains("cannot sign in to Google yet"), "{list}");
    let refused = ix.execute("/connect mail").await;
    assert!(
        refused
            .report
            .plain_text()
            .contains("no sign-in for Google"),
        "{}",
        refused.report.plain_text()
    );
    let tools = ix.execute("/tools").await.report.plain_text();
    assert!(
        tools.contains("web.search") && tools.contains("web.fetch"),
        "{tools}"
    );
}

/// A provider and a mail API on loopback. The sign-in page sends the browser straight back
/// with a code; the token endpoint answers for `ada@example.com` with Mail's scope; the API
/// records each message it is asked to send.
async fn fake_google() -> (String, String, Arc<std::sync::Mutex<Vec<String>>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let sent: Arc<std::sync::Mutex<Vec<String>>> = Arc::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let log = sent.clone();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let log = log.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 65536];
                let mut n = 0;
                // Read the head and, for a POST, its body.
                loop {
                    let k = sock.read(&mut buf[n..]).await.unwrap_or(0);
                    n += k;
                    let text = String::from_utf8_lossy(&buf[..n]).to_string();
                    let Some(end) = text.find("\r\n\r\n") else {
                        if k == 0 {
                            break;
                        } else {
                            continue;
                        }
                    };
                    let len = text[..end]
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                        })
                        .unwrap_or(0);
                    if n >= end + 4 + len || k == 0 {
                        break;
                    }
                }
                let text = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = text.split_whitespace().nth(1).unwrap_or("/").to_string();
                let body = text.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
                let reply = if path.starts_with("/auth") {
                    let url = reqwest::Url::parse(&format!("http://x{path}")).unwrap();
                    let get = |k: &str| {
                        url.query_pairs()
                            .find(|(n, _)| n == k)
                            .unwrap()
                            .1
                            .to_string()
                    };
                    format!(
                        "HTTP/1.1 302 Found\r\nlocation: {}?code=c0de&state={}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                        get("redirect_uri"),
                        get("state")
                    )
                } else {
                    let out = if path.starts_with("/token") {
                        serde_json::json!({
                            "access_token": "a", "refresh_token": "r", "expires_in": 3600,
                            "scope": "openid email https://www.googleapis.com/auth/gmail.send",
                            "id_token": "e30.eyJlbWFpbCI6ImFkYUBleGFtcGxlLmNvbSJ9.sig"
                        })
                    } else if path.ends_with("/messages/send") {
                        log.lock().unwrap().push(body);
                        serde_json::json!({ "id": "m1" })
                    } else {
                        serde_json::json!({})
                    }
                    .to_string();
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{out}",
                        out.len()
                    )
                };
                let _ = sock.write_all(reply.as_bytes()).await;
            });
        }
    });
    (base.clone(), base, sent)
}

fn call(name: &str, args: serde_json::Value) -> Completion {
    Completion {
        content: String::new(),
        tool_calls: vec![ToolCall {
            id: format!("c-{name}"),
            name: name.into(),
            arguments: args,
        }],
        usage: Usage::measured(50, 5),
    }
}

/// The loop the product is about: the agent needs Mail, which is not connected; it asks; the
/// person sees "Connect Google so ARK can send email", says yes, signs in; the same turn goes on
/// and sends the mail, which the person approves as usual.
#[tokio::test]
async fn an_agent_asks_for_mail_mid_task_and_sends_once_it_is_connected() {
    use ark_sdk::prelude::ark_connect::{Catalog, Connections, MemorySecrets};
    let (provider, api, sent) = fake_google().await;
    let dir = tempfile::tempdir().unwrap();
    let mut catalog = Catalog::builtin();
    let g = catalog
        .providers
        .iter_mut()
        .find(|p| p.id == "google")
        .unwrap();
    g.oauth.auth_url = format!("{provider}/auth");
    g.oauth.token_url = format!("{provider}/token");
    g.oauth.revoke_url = None;
    g.oauth.client_id = "test".into();
    g.oauth.client_id_env = None;
    let conns = Connections::open(dir.path(), catalog, Arc::new(MemorySecrets::default()))
        .with_base("google.mail", &api);
    let model = Arc::new(MockProvider::new(
        "m",
        vec![
            call("acquire_capability", serde_json::json!({ "need": "mail" })),
            call(
                "mail.send",
                serde_json::json!({ "to": ["bob@example.com"], "subject": "Report", "body": "It is ready." }),
            ),
            Completion {
                content: "Sent the report to Bob.".into(),
                tool_calls: vec![],
                usage: Usage::measured(60, 6),
            },
        ],
    ));
    let ark = Ark::builder()
        .store_root(dir.path())
        .provider(model.clone())
        .connections_with(conns)
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let (approver, mut rx) = ChannelApprover::channel();
    // The person: says yes to everything, and remembers what they were asked.
    let asked: Arc<std::sync::Mutex<Vec<(String, String)>>> = Arc::default();
    let seen = asked.clone();
    tokio::spawn(async move {
        while let Some(p) = rx.recv().await {
            seen.lock().unwrap().push((
                ark_interact::approval_title(&p.request),
                p.request.summary.clone(),
            ));
            p.answer(ApprovalDecision::AllowOnce);
        }
    });
    // The browser: follows the sign-in page back to ARK.
    let browser: ark_interact::capabilities::Opener = Arc::new(|url: &str| {
        let url = url.to_string();
        tokio::spawn(async move { reqwest::get(url).await.map(|_| ()) });
        true
    });
    let mut ix = App::new("Test")
        .ark(ark)
        .sign_in(browser)
        .open(&Start::default(), Arc::new(approver))
        .unwrap()
        .interaction;

    let end = ix
        .run_turn(
            "Email bob@example.com that the report is ready",
            &TurnControl::new(),
            |_, _| {},
        )
        .await;
    let answer = end.result.unwrap().answer;
    assert!(answer.contains("Sent"), "{answer}");

    let asked = asked.lock().unwrap().clone();
    assert_eq!(asked.len(), 2, "{asked:?}");
    assert_eq!(asked[0].0, "ARK needs this to finish:");
    assert!(
        asked[0]
            .1
            .starts_with("Connect Google so ARK can send email from your account"),
        "{asked:?}"
    );
    assert_eq!(sent.lock().unwrap().len(), 1, "the mail went out once");

    let requests = model.recorded();
    let first = &requests[0].messages[0].content;
    assert!(
        first.contains("== CAN BE ADDED ==") && first.contains("- mail: send email"),
        "{first}"
    );
    assert!(!requests[0].tools.iter().any(|t| t.name == "mail.send"));
    assert!(
        requests[1].tools.iter().any(|t| t.name == "mail.send"),
        "Mail joined the turn"
    );
    let list = ix.execute("/connect").await.report.plain_text();
    let mail = list.lines().find(|l| l.contains("Mail")).unwrap_or("");
    assert!(
        mail.trim_start().starts_with('✓') && mail.contains("ada@example.com"),
        "{list}"
    );
}
