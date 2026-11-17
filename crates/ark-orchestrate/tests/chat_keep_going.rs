//! A chat turn is nudged once on a one-line note and has 40 steps (ADR 0125). Scripted model.

use std::sync::{Arc, Mutex};

use ark_model::{Completion, Message, Provider, Role, ToolCall, ToolDef, Usage};
use ark_orchestrate::*;
use ark_runtime::session::Session;
use ark_sdk::prelude::*;
use serde_json::{json, Value};

const NOTE: &str = "Next I'm updating the packaging script.";
const FINAL: &str = "The Spanish downloads are built.";
const NUDGE: &str = "Your last reply was one short line";

fn reply(text: &str, name: &str, args: Value) -> Completion {
    let tool_calls = if name.is_empty() {
        vec![]
    } else {
        vec![ToolCall {
            id: format!("c-{}", args),
            name: name.into(),
            arguments: args,
        }]
    };
    Completion {
        content: text.into(),
        tool_calls,
        usage: Usage::measured(10, 5),
    }
}

/// "zzkeep": note, nudge, call, answer. "zzclose <tool>": a call with text, then a short line.
/// "zzloop": varied reads without end. Any other request is background work.
struct Model {
    seen: Mutex<Vec<usize>>,
    loops: Mutex<usize>,
}

#[async_trait::async_trait]
impl Provider for Model {
    async fn complete(
        &self,
        _: &str,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> ark_model::Result<Completion> {
        let said = messages
            .iter()
            .filter(|m| m.role == Role::User && m.tool_name.is_none())
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let results = messages.iter().filter(|m| m.tool_name.is_some()).count();
        let nudged = said.contains(NUDGE);
        self.seen.lock().unwrap().push(messages.len());
        if said.contains("zzloop") {
            if tools.is_empty() {
                return Ok(reply("where I got to", "", json!({})));
            }
            let mut n = self.loops.lock().unwrap();
            *n += 1;
            return Ok(reply(
                "",
                "look.read",
                json!({ "path": format!("p{}", *n) }),
            ));
        }
        if said.contains("zzkeep") {
            return Ok(match (results, nudged) {
                (0, _) => reply("Reading the file.", "look.read", json!({"path": "a"})),
                (_, false) => reply(NOTE, "", json!({})),
                (1, true) => reply("", "look.read", json!({"path": "b"})),
                _ => reply(FINAL, "", json!({})),
            });
        }
        if let Some(tool) = said
            .split_once("zzclose ")
            .map(|(_, t)| t.split_whitespace().next().unwrap_or(""))
        {
            let args = match tool {
                "role.set" => json!({"instructions": "Be brief."}),
                "work.later" => json!({"brief": "look into it"}),
                "groups.set" => json!({"name": "Ops", "members": [{"name": "ops", "role": "Ops"}]}),
                "agents.schedule" => json!({"objective": "check", "at": "09:00"}),
                _ => json!({"schedule": "s-none"}),
            };
            return Ok(if results == 0 {
                reply("Setting it.", tool, args)
            } else {
                reply("I will.", "", json!({}))
            });
        }
        Ok(reply("background done", "", json!({})))
    }
    fn describe(&self) -> String {
        "m".into()
    }
}

struct Look;

#[async_trait::async_trait]
impl Adapter for Look {
    fn name(&self) -> &str {
        "look"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![ActionDescriptor::new("look.read", "read", RiskTier::Read)]
    }
    async fn run(&self, _: &str, _: Value, _: &RunCtx<'_>) -> ark_tools::Result<ActionOutput> {
        Ok(ActionOutput::json(json!({ "text": "x" })))
    }
}

struct World {
    _dir: tempfile::TempDir,
    ark: Ark,
    o: Orchestrator,
    m: Arc<Model>,
}

async fn world() -> World {
    let m = Arc::new(Model {
        seen: Mutex::new(vec![]),
        loops: Mutex::new(0),
    });
    let dir = tempfile::tempdir().unwrap();
    let ark = Ark::builder()
        .store_root(dir.path())
        .tool(Arc::new(Look))
        .provider(Arc::new(ark_model::MockProvider::new("unused", vec![])))
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.create_project("team", "a test").unwrap();
    let mm = m.clone();
    let o = Orchestrator::builder(&ark, &project)
        .configure_models(move |b| b.provider("m", mm as Arc<dyn Provider>))
        .build()
        .unwrap();
    o.spawn(AgentSpec::new("ceo").model("m")).await.unwrap();
    World {
        _dir: dir,
        ark,
        o,
        m,
    }
}

fn said(text: &str) -> TaskSpec {
    let mut t = TaskSpec::new(text);
    t.person = true;
    t
}

fn stored(w: &World, chat: &str) -> Vec<String> {
    Session::open(w.ark.engine(), chat)
        .unwrap()
        .unwrap()
        .transcript_page(w.ark.engine(), None, 10)
        .unwrap()
        .into_iter()
        .map(|t| t.answer)
        .collect()
}

#[tokio::test]
async fn a_chat_turn_and_a_group_turn_that_end_on_a_note_are_nudged_and_store_one_answer() {
    for group in [false, true] {
        let w = world().await;
        let chat = w.o.start_thread("ceo").unwrap();
        let mut spec = if group {
            let mut t = TaskSpec::new("zzkeep");
            t.group_turn = true;
            t
        } else {
            said("zzkeep")
        };
        spec = spec.thread(&chat);
        w.o.run("ceo", spec).await.unwrap();
        assert_eq!(
            w.m.seen.lock().unwrap().len(),
            5,
            "group={group}: note, nudge, call, a one-line answer sent back too (ADR 0127), answer"
        );
        assert_eq!(stored(&w, &chat), vec![FINAL.to_string()], "group={group}");
    }
}

#[tokio::test]
async fn the_tools_that_ask_for_a_short_closing_line_are_not_nudged() {
    for tool in [
        "role.set",
        "work.later",
        "groups.set",
        "agents.schedule",
        "agents.unschedule",
    ] {
        let w = world().await;
        let chat = w.o.start_thread("ceo").unwrap();
        let before = w.m.seen.lock().unwrap().len();
        w.o.run("ceo", said(&format!("zzclose {tool}")).thread(&chat))
            .await
            .unwrap();
        let answers = stored(&w, &chat);
        assert_eq!(
            answers,
            vec!["I will.".to_string()],
            "{tool}: {:?}",
            w.m.seen.lock().unwrap().len() - before
        );
    }
}

#[tokio::test]
async fn chat_and_started_work_have_40_steps_and_scheduled_work_16() {
    let w = world().await;
    let chat = w.o.start_thread("ceo").unwrap();
    w.o.run("ceo", said("zzloop chat").thread(&chat))
        .await
        .unwrap();
    assert_eq!(*w.m.loops.lock().unwrap(), 40, "the person's chat turn");
    *w.m.loops.lock().unwrap() = 0;
    w.o.run("ceo", TaskSpec::new("zzloop started"))
        .await
        .unwrap();
    assert_eq!(*w.m.loops.lock().unwrap(), 40, "started work");
    *w.m.loops.lock().unwrap() = 0;
    w.o.run(
        "ceo",
        TaskSpec::new("zzloop capped").budget(Budget::default().steps(5)),
    )
    .await
    .unwrap();
    assert_eq!(*w.m.loops.lock().unwrap(), 5, "an explicit limit wins");
    *w.m.loops.lock().unwrap() = 0;
    let s =
        w.o.schedule(
            "ceo",
            TaskSpec::new("zzloop scheduled"),
            When::every_minutes(10),
        )
        .unwrap();
    let id = w.o.tick_schedules(s.next_ms.unwrap()).await.remove(0);
    w.o.wait(&id).await.unwrap();
    assert_eq!(*w.m.loops.lock().unwrap(), 16, "a scheduled run");
}
