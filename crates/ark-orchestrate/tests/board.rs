//! The project's boards driven by scripted models: the conversation plans on the board of a
//! goal and hands items to agents, the items follow their tasks, two agents never take one item,
//! each goal keeps its own board, and the boards survive a restart. No model or CLI is needed.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use ark_model::{Completion, Message, Provider, ProviderError, ToolCall, ToolDef, Usage};
use ark_orchestrate::board::PERSON;
use ark_orchestrate::*;
use ark_sdk::prelude::*;
use serde_json::json;
use tokio::sync::Notify;

/// A scripted model that records each request (text and tools offered).
struct Script {
    id: String,
    replies: Mutex<VecDeque<Completion>>,
    seen: Mutex<Vec<String>>,
    tools: Mutex<Vec<Vec<ToolDef>>>,
    /// Calls that fail before the script answers.
    fail: AtomicUsize,
    gate: Option<Arc<Notify>>,
}

impl Script {
    fn new(id: &str, replies: Vec<Completion>) -> Arc<Self> {
        Arc::new(Script {
            id: id.into(),
            replies: Mutex::new(replies.into()),
            seen: Mutex::new(Vec::new()),
            tools: Mutex::new(Vec::new()),
            fail: AtomicUsize::new(0),
            gate: None,
        })
    }
    fn gated(id: &str, replies: Vec<Completion>, gate: Arc<Notify>) -> Arc<Self> {
        Arc::new(Script {
            gate: Some(gate),
            ..Arc::into_inner(Script::new(id, replies)).unwrap()
        })
    }
    fn texts(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }
    fn push(&self, more: Vec<Completion>) {
        self.replies.lock().unwrap().extend(more);
    }
}

#[async_trait::async_trait]
impl Provider for Script {
    async fn complete(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> ark_model::Result<Completion> {
        let mut t = system.to_string();
        for m in messages {
            t.push('\n');
            t.push_str(&m.content);
        }
        self.seen.lock().unwrap().push(t);
        self.tools.lock().unwrap().push(tools.to_vec());
        if let Some(g) = &self.gate {
            g.notified().await;
        }
        if self
            .fail
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
        {
            return Err(ProviderError::Api {
                provider: self.id.clone(),
                detail: "the model is down".into(),
            });
        }
        let next = self.replies.lock().unwrap().pop_front();
        Ok(next.unwrap_or_else(|| answer("(no more script)")))
    }
    fn describe(&self) -> String {
        self.id.clone()
    }
}

fn answer(text: &str) -> Completion {
    Completion {
        content: text.into(),
        tool_calls: vec![],
        usage: Usage::measured(100, 20),
    }
}

fn calls(calls: Vec<(&str, serde_json::Value)>) -> Completion {
    Completion {
        content: String::new(),
        tool_calls: calls
            .into_iter()
            .enumerate()
            .map(|(i, (name, args))| ToolCall {
                id: format!("c{i}"),
                name: name.into(),
                arguments: args,
            })
            .collect(),
        usage: Usage::measured(100, 20),
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    ark: Ark,
    project: Project,
}

fn fixture(chat: Arc<Script>) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let ark = Ark::builder()
        .store_root(dir.path())
        .provider(chat as Arc<dyn Provider>)
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.create_project("novel", "").unwrap();
    Fixture {
        _dir: dir,
        ark,
        project,
    }
}

fn orch(f: &Fixture, models: Vec<(&str, Arc<Script>)>) -> Orchestrator {
    Orchestrator::builder(&f.ark, &f.project)
        .configure_models(move |mut b| {
            for (name, p) in models {
                b = b.provider(name, p as Arc<dyn Provider>);
            }
            b
        })
        .build()
        .unwrap()
}

fn titles(o: &Orchestrator, state: WorkState) -> Vec<String> {
    o.board()
        .into_iter()
        .filter(|w| w.state == state)
        .map(|w| w.title)
        .collect()
}

#[tokio::test]
async fn the_conversation_plans_on_the_board_and_the_items_follow_the_work() {
    let chat = Script::new(
        "chat",
        vec![
            calls(vec![("board.list", json!({}))]),
            // An id that names no board is refused, not taken for a goal.
            calls(vec![(
                "board.add",
                json!({ "board": "b-00000000", "items": [{ "title": "Draft chapter 3" }] }),
            )]),
            calls(vec![(
                "board.add",
                json!({ "board": "Chapters", "items": [
                    { "title": "Draft chapter 3", "note": "Under 2,000 words." },
                    { "title": "Edit chapter 2" },
                    { "title": "Pick a title", "owner": "user" }
                ] }),
            )]),
            answer("Three items are on the board."),
        ],
    );
    let f = fixture(chat.clone());
    let writer = Script::new("writer", vec![answer("Chapter 3 is drafted in ch3.md.")]);
    let editor = Script::new("editor", vec![answer("Chapter 2 is edited.")]);
    let o = orch(
        &f,
        vec![("writer", writer.clone()), ("editor", editor.clone())],
    );
    o.spawn(
        AgentSpec::new("writer")
            .role("drafts chapters")
            .model("writer"),
    )
    .await
    .unwrap();
    o.spawn(
        AgentSpec::new("editor")
            .role("edits chapters")
            .model("editor"),
    )
    .await
    .unwrap();
    let mut session = f.ark.start_session(&f.project.id).unwrap();
    let opts = || TurnOptions {
        tools: vec![o.chat_tools(), o.board_tools()],
        ..Default::default()
    };
    f.ark
        .ask_session(&mut session, "plan the next chapters", opts(), |_| {})
        .await
        .unwrap();
    // The boards read as a summary in their tool's description; none at first.
    let offered = chat.tools.lock().unwrap()[0].clone();
    let list = offered.iter().find(|t| t.name == "board.list").unwrap();
    assert!(
        list.description.contains("There are no boards yet."),
        "{}",
        list.description
    );
    // The goal made a board that remembers the chat it was planned in.
    let boards = o.boards();
    assert_eq!(boards.len(), 1);
    assert_eq!(boards[0].goal, "Chapters");
    assert_eq!(boards[0].chat.as_deref(), Some(session.id().as_str()));
    assert_eq!(
        titles(&o, WorkState::Todo),
        ["Draft chapter 3", "Edit chapter 2", "Pick a title"]
    );
    let board = o.board();
    assert!(board
        .iter()
        .all(|w| w.by == CHAT && w.board == boards[0].id));
    assert_eq!(board[2].owner.as_deref(), Some(PERSON));
    let (draft, edit) = (board[0].id.clone(), board[1].id.clone());

    // Next turn: hand two items to two agents, in parallel.
    chat.push(vec![
        calls(vec![
            (
                "agents.delegate",
                json!({ "to": "writer", "objective": "Draft chapter 3", "item": draft, "wait": false }),
            ),
            (
                "agents.delegate",
                json!({ "to": "editor", "objective": "Edit chapter 2", "item": edit, "wait": false }),
            ),
        ]),
        answer("Both are working."),
    ]);
    f.ark
        .ask_session(&mut session, "start on them", opts(), |_| {})
        .await
        .unwrap();
    let tasks: Vec<String> = o.board().iter().flat_map(|w| w.tasks.clone()).collect();
    assert_eq!(tasks.len(), 2);
    for t in &tasks {
        o.wait(t).await.unwrap();
    }

    // The items followed their tasks: owner, state and a short history, with no call for it.
    let w = o.work_item(&draft).unwrap();
    assert_eq!(
        (w.state, w.owner.as_deref()),
        (WorkState::Done, Some("writer"))
    );
    let steps: Vec<&str> = w.log.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(steps[0], "added it");
    assert!(steps[1].starts_with(&format!("started {}", w.tasks[0])));
    assert!(steps[2].contains("finished") && steps[2].contains("Chapter 3 is drafted"));
    assert_eq!(titles(&o, WorkState::Todo), ["Pick a title"]);
    // The worker's brief carried its item and its note; never the chat.
    let brief = writer.texts()[0].clone();
    assert!(
        brief.contains(&format!("== BOARD ITEM ==\n{draft} Draft chapter 3")),
        "{brief}"
    );
    assert!(brief.contains("Note: Under 2,000 words."));
    assert!(!brief.contains("plan the next chapters"));

    // The next turn's description says where the project stands.
    chat.push(vec![answer("ok")]);
    f.ark
        .ask_session(&mut session, "where are we", opts(), |_| {})
        .await
        .unwrap();
    let offered = chat.tools.lock().unwrap().last().unwrap().clone();
    let list = offered.iter().find(|t| t.name == "board.list").unwrap();
    assert!(
        list.description
            .contains("Boards: \"Chapters\" (1 to do, 0 in progress, 2 done)."),
        "{}",
        list.description
    );

    // The board is part of the trace; the stream keeps what changed, not each item's history.
    let events = o.events(0, 500).unwrap();
    let work: Vec<&WorkItem> = events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::Work { item, .. } => Some(item),
            _ => None,
        })
        .collect();
    assert!(work.len() >= 7);
    assert!(work.iter().all(|w| w.log.len() == 1));

    // A restart keeps the board as it was; a new session reads it back.
    drop(o);
    let o = orch(&f, vec![("writer", writer), ("editor", editor)]);
    assert_eq!(titles(&o, WorkState::Done).len(), 2);
    assert_eq!(o.work_item(&draft).unwrap().log.len(), 3);
    let mut fresh = f.ark.start_session(&f.project.id).unwrap();
    chat.push(vec![
        calls(vec![("board.list", json!({}))]),
        answer("Pick a title is left."),
    ]);
    f.ark
        .ask_session(
            &mut fresh,
            "what is left?",
            TurnOptions {
                tools: vec![o.chat_tools(), o.board_tools()],
                ..Default::default()
            },
            |_| {},
        )
        .await
        .unwrap();
    let last = chat.texts().last().unwrap().clone();
    assert!(last.contains("Board \"Chapters\" (b-"), "{last}");
    assert!(last.contains("[todo, user] Pick a title"), "{last}");
    assert!(last.contains("[done, writer] Draft chapter 3"), "{last}");
}

#[tokio::test]
async fn two_agents_never_take_one_item() {
    let f = fixture(Script::new("chat", vec![]));
    let gate = Arc::new(Notify::new());
    let writer = Script::gated("writer", vec![answer("drafted")], gate.clone());
    let editor = Script::new("editor", vec![answer("x")]);
    let o = orch(&f, vec![("writer", writer), ("editor", editor)]);
    o.spawn(AgentSpec::new("writer").model("writer"))
        .await
        .unwrap();
    o.spawn(AgentSpec::new("editor").model("editor"))
        .await
        .unwrap();
    let w = o
        .add_work(PERSON, "Draft chapter 3", WorkChange::default())
        .unwrap();
    let t = o
        .submit("writer", TaskSpec::new("Draft chapter 3").item(&w.id))
        .unwrap();
    let err = o
        .submit("editor", TaskSpec::new("Draft chapter 3").item(&w.id))
        .unwrap_err()
        .to_string();
    assert!(err.contains("being worked on by writer"), "{err}");
    // Nor can it be given to someone else while the writer works on it.
    let err = o
        .change_work(PERSON, &w.id, WorkChange::default().owner("editor"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("writer is working on it"), "{err}");
    assert!(o
        .submit("editor", TaskSpec::new("x").item("w-nothere"))
        .is_err());
    gate.notify_one();
    o.wait(&t).await.unwrap();
    assert_eq!(o.work_item(&w.id).unwrap().state, WorkState::Done);
    // Once it is done, it can be handed on.
    let t2 = o
        .submit("editor", TaskSpec::new("Edit chapter 3").item(&w.id))
        .unwrap();
    let w2 = o.work_item(&w.id).unwrap();
    assert_eq!(
        (w2.state, w2.owner.as_deref()),
        (WorkState::Doing, Some("editor"))
    );
    o.wait(&t2).await.unwrap();
}

#[tokio::test]
async fn a_failure_blocks_the_item_a_retry_clears_it_and_a_cancel_puts_it_back() {
    let f = fixture(Script::new("chat", vec![]));
    let writer = Script::new("writer", vec![answer("drafted at last")]);
    writer.fail.store(3, Ordering::SeqCst);
    let o = orch(&f, vec![("writer", writer.clone())]);
    o.spawn(AgentSpec::new("writer").model("writer"))
        .await
        .unwrap();
    let w = o
        .add_work(PERSON, "Draft chapter 3", WorkChange::default())
        .unwrap();
    let t = o
        .submit("writer", TaskSpec::new("Draft chapter 3").item(&w.id))
        .unwrap();
    let done = o.wait(&t).await.unwrap();
    assert_eq!(done.state, TaskState::Failed);
    let after = o.work_item(&w.id).unwrap();
    assert_eq!(after.state, WorkState::Doing);
    let blocked = after.blocked.clone().unwrap();
    assert!(blocked.starts_with(&format!("{t} failed")), "{blocked}");
    // Blocked on an agent: not the person's to answer.
    assert!(!after.needs_person());
    writer.fail.store(0, Ordering::SeqCst);
    o.retry(&t).unwrap();
    assert!(o.work_item(&w.id).unwrap().blocked.is_none());
    o.wait(&t).await.unwrap();
    assert_eq!(o.work_item(&w.id).unwrap().state, WorkState::Done);

    // A cancelled run puts its item back in To do.
    let gate = Arc::new(Notify::new());
    let slow = Script::gated("slow", vec![answer("x")], gate);
    let o2 = orch(&f, vec![("slow", slow)]);
    o2.spawn(AgentSpec::new("slow").model("slow"))
        .await
        .unwrap();
    let w = o2
        .add_work(PERSON, "Outline chapter 4", WorkChange::default())
        .unwrap();
    let t = o2
        .submit("slow", TaskSpec::new("Outline chapter 4").item(&w.id))
        .unwrap();
    assert_eq!(o2.work_item(&w.id).unwrap().state, WorkState::Doing);
    o2.cancel(&t).unwrap();
    o2.wait(&t).await.unwrap();
    let w = o2.work_item(&w.id).unwrap();
    assert_eq!(w.state, WorkState::Todo);
    assert!(w.log.last().unwrap().text.ends_with("was cancelled"));
}

#[tokio::test]
async fn the_person_orders_and_changes_the_board() {
    let f = fixture(Script::new("chat", vec![]));
    let o = orch(&f, vec![]);
    let a = o.add_work(PERSON, "A", WorkChange::default()).unwrap();
    let b = o.add_work(PERSON, "B", WorkChange::default()).unwrap();
    let c = o
        .add_work(PERSON, "C", WorkChange::default().before(&a.id))
        .unwrap();
    assert_eq!(titles(&o, WorkState::Todo), ["C", "A", "B"]);
    o.change_work(PERSON, &b.id, WorkChange::default().before(&a.id))
        .unwrap();
    assert_eq!(titles(&o, WorkState::Todo), ["C", "B", "A"]);
    o.change_work(PERSON, &c.id, WorkChange::default().state(WorkState::Doing))
        .unwrap();
    o.change_work(PERSON, &a.id, WorkChange::default().state(WorkState::Done))
        .unwrap();
    assert_eq!(titles(&o, WorkState::Todo), ["B"]);
    assert_eq!(titles(&o, WorkState::Doing), ["C"]);
    let blocked = o
        .change_work(
            PERSON,
            &b.id,
            WorkChange::default().blocked("the cover art").owner("user"),
        )
        .unwrap();
    assert!(blocked.needs_person());
    let cleared = o
        .change_work(PERSON, &b.id, WorkChange::default().blocked(""))
        .unwrap();
    assert!(cleared.blocked.is_none());
    assert!(o
        .change_work(PERSON, &b.id, WorkChange::default().title("  "))
        .is_err());
    assert!(o
        .change_work(PERSON, &b.id, WorkChange::default().owner("nobody-here"))
        .is_err());
    o.remove_work(&c.id).unwrap();
    assert!(o.work_item(&c.id).is_none());
    drop(o);
    let o = orch(&f, vec![]);
    let all: Vec<String> = o.board().into_iter().map(|w| w.title).collect();
    assert_eq!(all, ["B", "A"]);
}

#[tokio::test]
async fn each_goal_has_its_own_board() {
    let f = fixture(Script::new("chat", vec![]));
    let o = orch(&f, vec![]);
    let site = o.add_board(CHAT, "Launch the site", None).unwrap();
    // The same goal in other case and spacing is the same board.
    assert_eq!(
        o.add_board(PERSON, " launch  the SITE", None).unwrap(),
        site
    );
    let book = o.add_board(PERSON, "Chapter 3", None).unwrap();
    let on = |b: &str| WorkChange::default().board(b);
    let copy = o.add_work(PERSON, "Write the copy", on(&site.id)).unwrap();
    let draft = o.add_work(PERSON, "Draft it", on(&book.id)).unwrap();
    // Each board orders its own columns.
    let logo = o
        .add_work(PERSON, "Pick a logo", on(&site.id).before(&copy.id))
        .unwrap();
    let column = |b: &str| -> Vec<String> {
        o.board()
            .into_iter()
            .filter(|w| w.board == b)
            .map(|w| w.title)
            .collect()
    };
    assert_eq!(column(&site.id), ["Pick a logo", "Write the copy"]);
    assert_eq!(column(&book.id), ["Draft it"]);
    assert!(o.add_work(PERSON, "x", on("b-nothere")).is_err());
    // The latest activity comes first; a new item without a board goes on it.
    assert_eq!(o.boards()[0].id, site.id);
    let loose = o
        .add_work(PERSON, "Buy the domain", WorkChange::default())
        .unwrap();
    assert_eq!(loose.board, site.id);

    // An item moves to another board, at the end of its column, and says so.
    let moved = o.change_work(PERSON, &logo.id, on(&book.id)).unwrap();
    assert_eq!(column(&book.id), ["Draft it", "Pick a logo"]);
    assert_eq!(
        moved.log.last().unwrap().text,
        "moved it to the board \"Chapter 3\""
    );

    // A goal changes; two boards never share one.
    let renamed = o
        .change_board(PERSON, &book.id, "Chapter 3, first draft")
        .unwrap();
    assert_eq!(renamed.goal, "Chapter 3, first draft");
    assert!(o.change_board(PERSON, &book.id, "Launch the site").is_err());

    // Removing a board removes its items, not the other board's.
    o.remove_board(&book.id).unwrap();
    assert!(o.work_item(&draft.id).is_none());
    assert_eq!(o.boards().len(), 1);
    let events = o.events(0, 500).unwrap();
    assert!(events
        .iter()
        .any(|e| matches!(&e.kind, EventKind::BoardRemoved { board, .. } if *board == book.id)));
    assert!(events
        .iter()
        .any(|e| matches!(&e.kind, EventKind::Board { board, .. } if board.id == site.id)));

    drop(o);
    let o = orch(&f, vec![]);
    assert_eq!(o.boards(), vec![site.clone()]);
    let left: Vec<String> = o.board().into_iter().map(|w| w.title).collect();
    assert_eq!(left, ["Write the copy", "Buy the domain"]);
}

#[tokio::test]
async fn items_made_before_boards_had_goals_join_one_board() {
    let f = fixture(Script::new("chat", vec![]));
    let item = |id: &str, title: &str| {
        json!({
            "id": id, "project": f.project.id.to_string(), "title": title,
            "state": "todo", "rank": 0.0, "by": "user", "created_ms": 1
        })
    };
    for (id, title) in [
        ("w-old00001", "Draft chapter 3"),
        ("w-old00002", "Edit chapter 2"),
    ] {
        f.ark
            .engine()
            .store
            .put_record("orch_work", id, f.project.id.as_str(), &item(id, title))
            .unwrap();
    }
    let o = orch(&f, vec![]);
    let boards = o.boards();
    assert_eq!(boards.len(), 1);
    assert_eq!(boards[0].goal, board::FIRST_GOAL);
    assert!(o.board().iter().all(|w| w.board == boards[0].id));
    // The move is kept: a restart finds the same board.
    drop(o);
    let o = orch(&f, vec![]);
    assert_eq!(o.boards(), boards);
}
