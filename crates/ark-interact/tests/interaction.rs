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

/// `/board`: the project's work by column; adding, handing to an agent, moving and removing go
/// through the same controls the window uses.
#[tokio::test]
async fn board_command_shows_and_changes_the_board() {
    use ark_interact::ark_orchestrate::{AgentSpec, Orchestrator, WorkState};
    let dir = tempfile::tempdir().unwrap();
    let worker: Arc<dyn ark_model::Provider> = Arc::new(MockProvider::new(
        "w",
        vec![Completion {
            content: "Chapter 3 is drafted.".into(),
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
    let project = ark.create_project("novel", "").unwrap();
    let orch = Orchestrator::builder(&ark, &project)
        .configure_models(|b| b.provider("w", worker))
        .build()
        .unwrap();
    orch.spawn(AgentSpec::new("writer").model("w"))
        .await
        .unwrap();
    let (approver, _rx) = ChannelApprover::channel();
    let mut ix = App::new("Novel")
        .ark(ark)
        .orchestrator(orch.clone())
        .open(&Start::default(), Arc::new(approver))
        .unwrap()
        .interaction;
    assert!(ix.commands().iter().any(|c| c.name == "board"));
    let empty = ix.execute("/board").await.report.plain_text();
    assert!(empty.contains("There are no boards yet"), "{empty}");
    let added = ix
        .execute("/board add Draft chapter 3")
        .await
        .report
        .plain_text();
    let id = orch.board()[0].id.clone();
    assert!(
        added.contains(&format!("{id} [todo] Draft chapter 3")),
        "{added}"
    );
    let given = ix
        .execute(&format!("/board give {id} writer"))
        .await
        .report
        .plain_text();
    assert!(given.contains("started task"), "{given}");
    let task = orch.work_item(&id).unwrap().tasks[0].clone();
    orch.wait(&task).await.unwrap();
    let text = ix.execute("/board").await.report.plain_text();
    let board = &orch.boards()[0];
    assert!(
        text.contains(&format!("Board · {}", board.id))
            && text.contains("Done (1)")
            && text.contains("writer"),
        "{text}"
    );
    ix.execute(&format!("/board move {id} to do")).await;
    assert_eq!(orch.work_item(&id).unwrap().state, WorkState::Todo);
    let bad = ix.execute("/board move").await.report.plain_text();
    assert!(bad.contains("usage: /board"), "{bad}");
    ix.execute(&format!("/board remove {id}")).await;
    assert!(orch.board().is_empty());
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
    // These tests are about questions: the person asked for them (a chat starts in Auto, ADR 0107).
    ix.set_execution_mode(0, ark_sdk::prelude::ark_core::ExecutionMode::Manual)
        .unwrap();

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

struct Drafts;
#[async_trait::async_trait]
impl Adapter for Drafts {
    fn name(&self) -> &str {
        "drafts"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![ActionDescriptor::new(
            "drafts.save",
            "save a draft",
            RiskTier::Mutate,
        )]
    }
    async fn run(
        &self,
        _a: &str,
        _i: serde_json::Value,
        _c: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        Ok(ActionOutput::json(serde_json::json!({ "saved": true })))
    }
}

/// "Always in this project" lasts across sessions of the project and shows in `/grants`; removed
/// there, the next save asks again.
#[tokio::test]
async fn an_always_grant_lasts_until_removed_with_grants() {
    let dir = tempfile::tempdir().unwrap();
    let saver = Arc::new(MockProvider::new("m", vec![]).with_fallback(|req| {
        // Saved once in this turn: answer (the working state is the last message, so look
        // for the save's result anywhere; a model that saved forever would run out of steps
        // and its next turn would continue it, ADR 0050).
        if req.messages.iter().any(|m| m.tool_call_id.is_some()) {
            Completion {
                content: "Saved.".into(),
                tool_calls: vec![],
                usage: Usage::measured(50, 5),
            }
        } else {
            call("drafts.save", serde_json::json!({}))
        }
    }));
    let ark = Ark::builder()
        .store_root(dir.path())
        .tool(Arc::new(Drafts))
        .provider(saver)
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let (approver, mut rx) = ChannelApprover::channel();
    let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = asked.clone();
    tokio::spawn(async move {
        while let Some(p) = rx.recv().await {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            p.answer(ApprovalDecision::AllowAlways);
        }
    });
    let mut ix = App::new("Test")
        .ark(ark)
        .open(&Start::default(), Arc::new(approver))
        .unwrap()
        .interaction;
    // These tests are about questions: the person asked for them (a chat starts in Auto, ADR 0107).
    ix.set_execution_mode(0, ark_sdk::prelude::ark_core::ExecutionMode::Manual)
        .unwrap();
    let n = || asked.load(std::sync::atomic::Ordering::SeqCst);

    ix.run_turn("save the draft", &TurnControl::new(), |_, _| {})
        .await;
    assert_eq!(n(), 1);
    ix.execute("/new").await;
    ix.set_execution_mode(0, ark_sdk::prelude::ark_core::ExecutionMode::Manual)
        .unwrap();
    ix.run_turn("save it again", &TurnControl::new(), |_, _| {})
        .await;
    assert_eq!(n(), 1, "a new session of the project did not ask");
    let list = ix.execute("/grants").await.report.plain_text();
    assert!(
        list.contains("Always in this project") && list.contains("drafts.save"),
        "{list}"
    );

    let removed = ix
        .execute("/grants remove drafts.save")
        .await
        .report
        .plain_text();
    assert!(removed.contains("Removed"), "{removed}");
    ix.run_turn("and once more", &TurnControl::new(), |_, _| {})
        .await;
    assert_eq!(n(), 2, "asked again after the grant was removed");
}

/// Two live conversations of one app at once: their own sessions and turns, the same agents.
/// A project with scheduled work gets its orchestrator when the app opens, with no chat there.
#[tokio::test]
async fn sibling_conversations_share_the_agents_and_scheduled_work_wakes_at_open() {
    use ark_interact::ark_orchestrate::{AgentSpec, Orchestrator, TaskSpec, When};
    use std::sync::Mutex;
    let dir = tempfile::tempdir().unwrap();
    let model: Arc<dyn ark_model::Provider> = Arc::new(
        MockProvider::new("m", vec![]).with_fallback(|req| Completion {
            content: format!(
                "answer to: {}",
                req.messages
                    .last()
                    .map(|m| m.content.as_str())
                    .unwrap_or("")
            ),
            tool_calls: vec![],
            usage: Usage::measured(50, 5),
        }),
    );
    let built: Arc<Mutex<Vec<String>>> = Arc::default();
    let app = |model: Arc<dyn ark_model::Provider>, built: Arc<Mutex<Vec<String>>>| {
        let ark = Ark::builder()
            .store_root(dir.path())
            .provider(model.clone())
            .planner(false)
            .critic(false)
            .build()
            .unwrap();
        App::new("Team")
            .ark(ark)
            .orchestrate(move |ark, project, gate| {
                built.lock().unwrap().push(project.name.clone());
                let m = model.clone();
                Ok(Orchestrator::builder(ark, project)
                    .configure_models(|b| b.provider("m", m).default_model("m"))
                    .approvals(gate)
                    .build()?)
            })
    };
    let (approver, _rx) = ChannelApprover::channel();
    let mut a = app(model.clone(), built.clone())
        .open(&Start::default(), Arc::new(approver))
        .unwrap()
        .interaction;
    let (approver2, _rx2) = ChannelApprover::channel();
    let mut b = a
        .siblings()
        .open(None, &a.project().id, Arc::new(approver2))
        .unwrap();
    assert_ne!(a.session().id(), b.session().id());
    assert_eq!(a.project().id, b.project().id);
    // Both run at once, each in its own session.
    let (ca, cb) = (TurnControl::new(), TurnControl::new());
    let (ea, eb) = tokio::join!(
        a.run_turn("first chat", &ca, |_, _| {}),
        b.run_turn("second chat", &cb, |_, _| {})
    );
    assert!(ea.result.unwrap().answer.contains("first chat"));
    assert!(eb.result.unwrap().answer.contains("second chat"));
    assert_eq!((a.session().turn_count(), b.session().turn_count()), (1, 1));
    // One orchestrator per project, whichever conversation asks.
    a.orchestrator()
        .unwrap()
        .spawn(AgentSpec::new("digest"))
        .await
        .unwrap();
    assert!(b.orchestrator().unwrap().agent("digest").is_some());
    assert_eq!(built.lock().unwrap().len(), 1);

    // Schedule work in another project, then open the app again in the first one.
    b.switch_project("evenings").unwrap();
    let o = b.orchestrator().unwrap().clone();
    o.spawn(AgentSpec::new("digest")).await.unwrap();
    o.schedule(
        "digest",
        TaskSpec::new("sum up the day"),
        When::daily_at("18:00"),
    )
    .unwrap();
    let report = b.execute("/schedules").await.report.plain_text();
    assert!(report.contains("every day at 18:00"), "{report}");
    drop((a, b, o));
    built.lock().unwrap().clear();
    let (approver, _rx) = ChannelApprover::channel();
    let start = Start {
        project: Some("team".into()),
        ..Default::default()
    };
    let _ix = app(model, built.clone())
        .open(&start, Arc::new(approver))
        .unwrap()
        .interaction;
    let woke = built.lock().unwrap().clone();
    assert!(woke.contains(&"evenings".to_string()), "{woke:?}");
}

/// ADR 0045, the conversation this came from: "open <an app> and write to <a chat>" with the
/// Computer off. The agent asks; the person sees "Allow ARK to use the apps on this computer", Allow and
/// Not now; the same turn goes on in the app. Once on, the next chat has the tools from its first
/// step; turned off, it asks again, also after an earlier "Always".
#[tokio::test]
async fn an_agent_asks_to_use_the_apps_and_the_turn_goes_on_in_the_app() {
    use ark_sdk::prelude::ark_computer::{fake::FakeScreen, ComputerService};
    let dir = tempfile::tempdir().unwrap();
    let screen = FakeScreen::new().app("Lumen", &[("row", "Notes"), ("text field", "Message")]);
    let svc = ComputerService::with_screen(Arc::new(screen.clone()))
        .timing(
            std::time::Duration::from_millis(50),
            std::time::Duration::from_millis(1),
        )
        .switched(dir.path().join("computer.json"));
    let model = Arc::new(MockProvider::new(
        "m",
        vec![
            // Turn 1: off, so it asks, then opens the app in the same turn.
            call(
                "acquire_capability",
                serde_json::json!({ "need": "use the apps on this computer" }),
            ),
            call("computer.open", serde_json::json!({ "app": "Lumen" })),
            Completion {
                content: "Lumen is open.".into(),
                tool_calls: vec![],
                usage: Usage::measured(60, 6),
            },
            Completion {
                content: "Lumen is open.".into(),
                tool_calls: vec![],
                usage: Usage::measured(60, 6),
            },
            // Turn 2: on, so the tools are there from the start.
            call("computer.open", serde_json::json!({ "app": "Lumen" })),
            Completion {
                content: "Again.".into(),
                tool_calls: vec![],
                usage: Usage::measured(60, 6),
            },
            Completion {
                content: "Again.".into(),
                tool_calls: vec![],
                usage: Usage::measured(60, 6),
            },
            // Turn 3: turned off, asks again; the person says Not now.
            call(
                "acquire_capability",
                serde_json::json!({ "need": "use the apps on this computer" }),
            ),
            Completion {
                content: "I need the Computer for that.".into(),
                tool_calls: vec![],
                usage: Usage::measured(60, 6),
            },
        ],
    ));
    let ark = Ark::builder()
        .store_root(dir.path())
        .provider(model.clone())
        .computer_with(svc.clone())
        // Even an app that lets agents change things unasked asks before turning it on.
        .allow_mutate(true)
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let (approver, mut rx) = ChannelApprover::channel();
    // Title, summary, answers offered, whether it asks each time, and its line.
    type Asked = (String, String, Vec<String>, bool, String);
    let asked: Arc<std::sync::Mutex<Vec<Asked>>> = Arc::default();
    let seen = asked.clone();
    tokio::spawn(async move {
        while let Some(p) = rx.recv().await {
            let n = {
                let mut s = seen.lock().unwrap();
                s.push((
                    ark_interact::approval_title(&p.request),
                    p.request.summary.clone(),
                    ark_interact::approval_choices(&p.request)
                        .into_iter()
                        .map(|c| c.label)
                        .collect(),
                    p.request.ask_each_time,
                    p.request.preview.clone(),
                ));
                s.len()
            };
            // First "Always" (no grant may cover a later ask), then Not now.
            p.answer(if n == 1 {
                ApprovalDecision::AllowAlways
            } else {
                ApprovalDecision::Deny
            });
        }
    });
    let mut ix = App::new("Test")
        .ark(ark)
        .open(&Start::default(), Arc::new(approver))
        .unwrap()
        .interaction;

    let list = ix.execute("/connect").await.report.plain_text();
    let row = list.lines().find(|l| l.contains("Computer")).unwrap_or("");
    assert!(
        row.trim_start().starts_with('○') && row.contains("/connect computer"),
        "{list}"
    );

    let end = ix
        .run_turn(
            "Open Lumen and write to Notes",
            &TurnControl::new(),
            |_, _| {},
        )
        .await;
    assert!(end.result.unwrap().answer.contains("open"));
    {
        let asked = asked.lock().unwrap();
        assert_eq!(asked.len(), 1, "{asked:?}");
        assert_eq!(asked[0].0, "ARK needs this to finish:");
        assert_eq!(asked[0].1, "It still asks before each step.");
        assert_eq!(asked[0].4, "Allow ARK to use the apps on this computer");
        assert_eq!(asked[0].2, ["Allow", "Not now"]);
        assert!(asked[0].3, "no grant covers a later request");
    }
    assert!(svc.is_on());
    assert!(
        screen
            .log
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.contains("open Lumen")),
        "{:?}",
        screen.log.lock().unwrap()
    );
    let requests = model.recorded();
    let first = &requests[0].messages[0].content;
    assert!(
        first.contains("- computer: use the apps on this computer"),
        "{first}"
    );
    assert!(!requests[0]
        .tools
        .iter()
        .any(|t| t.name.starts_with("computer.")));
    assert!(
        requests[1].tools.iter().any(|t| t.name == "computer.open"),
        "joined the turn"
    );

    // On: a new turn has the tools from its first step, and no offer.
    ix.run_turn("Open Lumen again", &TurnControl::new(), |_, _| {})
        .await
        .result
        .unwrap();
    let requests = model.recorded();
    let t2 = &requests[4];
    assert!(t2.tools.iter().any(|t| t.name == "computer.open"));
    assert!(!t2.messages[0].content.contains("- computer:"));
    assert_eq!(
        asked.lock().unwrap().len(),
        1,
        "nothing asked to turn it on again"
    );

    // Off: asks again although the first answer was "Always".
    let list = ix.execute("/disconnect computer").await.report.plain_text();
    assert!(list.contains("The Computer is off"), "{list}");
    assert!(!svc.is_on());
    ix.run_turn("Open Lumen", &TurnControl::new(), |_, _| {})
        .await
        .result
        .unwrap();
    assert_eq!(asked.lock().unwrap().len(), 2, "asked again after Turn off");
    assert!(!svc.is_on(), "Not now leaves it off");
}

/// The app's sources come first: "calendar app" is Calendar, "the apps on this computer" is
/// the Computer, and "computer" never reaches the account sign-ins.
#[tokio::test]
async fn a_need_reaches_the_source_that_serves_it() {
    use ark_sdk::prelude::ark_computer::{fake::FakeScreen, ComputerService};
    use ark_sdk::prelude::ark_connect::{Catalog, Connections, MemorySecrets};
    use ark_sdk::prelude::ark_runtime::per_agent::Owner;
    let dir = tempfile::tempdir().unwrap();
    let mut catalog = Catalog::builtin();
    for p in &mut catalog.providers {
        p.oauth.client_id = "test".into();
        p.oauth.client_id_env = None;
    }
    let conns = Connections::open(dir.path(), catalog, Arc::new(MemorySecrets::default()));
    let svc = ComputerService::with_screen(Arc::new(FakeScreen::new()))
        .switched(dir.path().join("computer.json"));
    let model: Arc<dyn ark_model::Provider> = Arc::new(MockProvider::new("m", vec![]));
    let ark = Ark::builder()
        .store_root(dir.path())
        .provider(model.clone())
        .connections_with(conns)
        .computer_with(svc)
        .build()
        .unwrap();
    let acq = ark
        .acquisition_for(
            ark.profile().acquisition.clone(),
            &Owner {
                name: "session:1",
                model: &model,
            },
        )
        .unwrap();
    assert!(acq
        .find("add it to my calendar app")
        .await
        .contains("Calendar"));
    assert!(acq
        .find("use the apps on this computer")
        .await
        .contains("The Computer"));
    assert!(acq.find("computer").await.contains("The Computer"));
    assert!(acq.asks_a_person("computer") && !acq.asks_a_person("mail"));
    let offer = acq.offer().unwrap();
    assert!(
        offer.contains("- mail:") && offer.contains("- computer:"),
        "{offer}"
    );
}

/// A need the app's own sources serve is theirs to ask, also when it says "app": with the
/// Computer on, "node to run the app" is the install question, never a silent yes.
#[tokio::test]
async fn an_install_that_names_an_app_is_asked_as_an_install() {
    use ark_sdk::prelude::ark_computer::{fake::FakeScreen, ComputerService};
    use ark_sdk::prelude::ark_proc::{Processes, Shell};
    let dir = tempfile::tempdir().unwrap();
    let svc = ComputerService::with_screen(Arc::new(FakeScreen::new()))
        .switched(dir.path().join("computer.json"));
    svc.set_on(true).unwrap();
    let mut env: Vec<(std::ffi::OsString, std::ffi::OsString)> =
        std::env::vars_os().filter(|(k, _)| k != "PATH").collect();
    env.push(("PATH".into(), "/usr/bin:/bin".into()));
    let procs =
        Processes::with_shell(dir.path().join("processes"), Shell::fixed("/bin/sh", env)).unwrap();
    let model = Arc::new(MockProvider::new(
        "m",
        vec![
            call(
                "acquire_capability",
                serde_json::json!({ "need": "node to run the app" }),
            ),
            Completion {
                content: "Node is needed.".into(),
                tool_calls: vec![],
                usage: Usage::measured(60, 6),
            },
        ],
    ));
    let ark = Ark::builder()
        .store_root(dir.path())
        .provider(model)
        .computer_with(svc)
        .processes_with(procs.clone())
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let (approver, mut rx) = ChannelApprover::channel();
    let asked: Arc<std::sync::Mutex<Vec<String>>> = Arc::default();
    let seen = asked.clone();
    tokio::spawn(async move {
        while let Some(p) = rx.recv().await {
            seen.lock().unwrap().push(p.request.summary.clone());
            p.answer(ApprovalDecision::Deny);
        }
    });
    let mut ix = App::new("Test")
        .ark(ark)
        .open(&Start::default(), Arc::new(approver))
        .unwrap()
        .interaction;
    ix.run_turn("Run the app", &TurnControl::new(), |_, _| {})
        .await
        .result
        .unwrap();
    let asked = asked.lock().unwrap().clone();
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert!(asked[0].starts_with("Install Node.js"), "{asked:?}");
    assert!(procs.list().is_empty(), "nothing installed");
}

#[test]
fn malformed_participant_history_does_not_hide_valid_chats() {
    let root = tempfile::tempdir().unwrap();
    let interaction = open(root.path());
    let summary = interaction.summary();
    interaction
        .engine()
        .store
        .put_record(
            "orch_task",
            "broken",
            &summary.project.id,
            &serde_json::json!({"id":"broken"}),
        )
        .unwrap();
    let chats =
        ark_interact::interaction::list_sessions(interaction.engine(), None, &summary.session.id)
            .unwrap();
    assert!(chats
        .iter()
        .any(|chat| chat.id == summary.session.id && chat.participants.is_none()));
}

#[tokio::test]
async fn project_memory_commands_review_replace_forget_and_survive_new_chats() {
    use ark_core::{Invocation, MemorySource, MemoryState};
    use ark_runtime::memory::MemoryReview;
    let dir = tempfile::tempdir().unwrap();
    let mut ix = open(dir.path());
    let empty = ix.execute("/memory").await.report.plain_text();
    assert!(empty.contains("No project memories yet"));
    let response = ix
        .execute("/memory remember report format | Use XLSX for reports.")
        .await
        .report
        .plain_text();
    assert!(response.contains("Remembered") && response.contains("Use XLSX"));
    let project = ix.project().id.clone();
    let first = ix
        .engine()
        .store
        .project_memories(&project)
        .unwrap()
        .remove(0);
    ix.execute("/new").await;
    let preview = ix.context_for_task("make a report").await.unwrap();
    assert!(preview.report.memory > 0);
    assert!(ix
        .execute("/context make a report")
        .await
        .report
        .plain_text()
        .contains("Project memory"));
    let invocation = Invocation::new("memory.suggest", RiskTier::Mutate, serde_json::json!({}));
    ix.engine()
        .store
        .put_invocation(&project, &invocation)
        .unwrap();
    let pending = ix
        .engine()
        .store
        .record_memory(
            &project,
            "report format",
            "Use CSV for reports.",
            MemorySource::Agent {
                invocation: invocation.id,
            },
            Some(first.id.clone()),
        )
        .unwrap();
    let review = ix
        .execute(&format!("/memory show {}", pending.id))
        .await
        .report;
    assert!(review.blocks.iter().any(|b| matches!(b, ark_interact::report::Block::Actions { actions } if actions.iter().any(|a| a.label == "Accept"))));
    assert!(review.blocks.iter().any(|b| matches!(b, ark_interact::report::Block::Actions { actions } if actions.iter().any(|a| a.label == "Reject"))));
    ix.execute(&format!("/memory accept {}", pending.id)).await;
    assert!(matches!(
        ix.engine()
            .store
            .project_memory(&project, first.id.as_str())
            .unwrap()
            .state,
        MemoryState::Superseded { .. }
    ));
    ix.execute(&format!(
        "/memory replace {} | Use TSV for reports.",
        pending.id
    ))
    .await;
    let current = ix
        .engine()
        .store
        .project_memories(&project)
        .unwrap()
        .into_iter()
        .find(|m| m.state == MemoryState::Active)
        .unwrap();
    ix.execute("/project other").await;
    let foreign = ix
        .execute(&format!("/memory forget {}", current.id))
        .await
        .report
        .plain_text();
    assert!(foreign.contains("memory not found in this project"));
    ix.execute(&format!("/project {project}")).await;
    assert_eq!(ix.project().id, project);
    let reviewer = ix.session().id().clone();
    ix.engine()
        .store
        .review_memory(
            &project,
            current.id.as_str(),
            MemoryReview::Forget,
            &reviewer,
        )
        .unwrap();
    assert_eq!(
        ix.context_for_task("make a report")
            .await
            .unwrap()
            .report
            .memory,
        0
    );
    let id = current.id.clone();
    drop(ix);
    let ix = open(dir.path());
    let forgotten = ix
        .engine()
        .store
        .project_memory(&project, id.as_str())
        .unwrap();
    assert_eq!(forgotten.state, MemoryState::Forgotten);
    assert!(forgotten.body.is_empty());
}

#[tokio::test]
async fn a_fresh_chat_receives_the_current_correction_and_preview_matches_the_request() {
    let dir = tempfile::tempdir().unwrap();
    let model = Arc::new(MockProvider::new("m", vec![]).with_fallback(|r| {
        Completion {
            content: if r.messages[0].content.contains("Use TSV for reports.") {
                "Use TSV for reports."
            } else {
                "Unknown: no report format is known."
            }
            .into(),
            tool_calls: vec![],
            usage: Usage::default(),
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
    let mut ix = App::new("Memory demo")
        .ark(ark)
        .open(&Start::default(), Arc::new(approver))
        .unwrap()
        .interaction;
    ix.execute("/memory remember report format | Use TSV for reports.")
        .await;
    ix.execute("/new").await;
    let preview = ix.context_for_task("write a report").await.unwrap().report;
    let outcome = ix
        .run_turn("write a report", &TurnControl::new(), |_, _| {})
        .await
        .result
        .unwrap();
    assert_eq!(outcome.answer, "Use TSV for reports.");
    let actual = ix.context_view().await.unwrap().report;
    assert_eq!(preview.memory, actual.memory);
    assert_eq!(preview.tool_schemas, actual.tool_schemas);
    assert!(model.recorded()[0].messages[0]
        .content
        .contains("== PROJECT MEMORY =="));
}

/// A stored conversation in `project` with one turn per entry of `turns`.
fn stored_chat(
    ix: &Interaction,
    project: &ProjectId,
    title: &str,
    turns: u32,
    work: bool,
) -> String {
    use ark_runtime::{CarryPolicy, SessionRecord};
    let id = ark_core::SessionId::new();
    let mut record =
        SessionRecord::empty(id.clone(), project.clone(), CarryPolicy::default(), None);
    record.title = title.into();
    record.turn_count = turns;
    if work {
        record.work_item = Some("w-1".into());
    }
    ix.engine()
        .store
        .create_session(
            id.as_str(),
            project,
            &serde_json::to_value(&record).unwrap(),
        )
        .unwrap();
    id.as_str().to_string()
}

fn listed(reply: &ark_interact::Reply) -> Vec<(String, String)> {
    reply
        .report
        .data
        .as_ref()
        .unwrap()
        .value
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["id"].as_str().unwrap().to_string(),
                s["project_name"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

#[tokio::test]
async fn history_lists_this_agents_conversations_across_top_projects() {
    let dir = tempfile::tempdir().unwrap();
    let mut ix = open(dir.path());
    let home = ix.project().clone();
    let second = ix.engine().create_project("second", "").unwrap();
    let mut scope = Project::new("helper", "");
    scope.parent = Some(home.id.clone());
    ix.engine().store.put_project(&scope).unwrap();

    let old = stored_chat(&ix, &home.id, "Harbour budget", 2, false);
    let other = stored_chat(&ix, &second.id, "Garden plan", 1, false);
    let newest = stored_chat(&ix, &home.id, "Latest", 1, false);
    let _empty = stored_chat(&ix, &second.id, "", 0, false);
    let _work = stored_chat(&ix, &home.id, "Harbour work", 3, true);
    let agent_old = stored_chat(&ix, &scope.id, "Helper harbour notes", 4, false);

    let reply = ix.execute("/history").await;
    assert!(
        reply.report.plain_text().contains("second"),
        "{}",
        reply.report.plain_text()
    );
    let rows = listed(&reply);
    let ids: Vec<_> = rows.iter().map(|(id, _)| id.clone()).collect();
    assert!(rows.contains(&(other.clone(), "second".to_string())));
    assert!(
        !ids.contains(&agent_old),
        "an agent's conversation is not ARK's"
    );
    let current = ix.session().id().as_str().to_string();
    assert_eq!(
        ids,
        vec![newest, other.clone(), old.clone(), current],
        "newest first across projects, no empty or work conversations, the current one kept"
    );

    let filtered = listed(&ix.execute("/history HARBOUR").await);
    assert_eq!(filtered, vec![(old.clone(), home.name.clone())]);

    // In an agent's scope, only that scope's conversations.
    assert!(ix.resume(&agent_old).unwrap());
    let rows = listed(&ix.execute("/history").await);
    assert_eq!(rows, vec![(agent_old.clone(), "helper".to_string())]);
    let row = &ix.execute("/history").await.report;
    assert!(!row.plain_text().contains("second"));
}
