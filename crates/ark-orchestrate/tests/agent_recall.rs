//! An agent's chats share its memory and can look back at each other on request: the later chat
//! finds what an earlier one settled through `conversations.search`, without that chat in its
//! prompt, and two chats suggesting memory at once both land. Driven by a scripted model.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use ark_model::{Completion, Message, Provider, ToolCall, ToolDef, Usage};
use ark_orchestrate::*;
use ark_sdk::prelude::*;
use serde_json::json;

/// One model call as the model saw it: everything it read, and the tools it was given.
#[derive(Clone, Debug)]
struct Seen {
    text: String,
    tools: Vec<String>,
}

struct Script {
    replies: Mutex<VecDeque<Completion>>,
    seen: Mutex<Vec<Seen>>,
}

#[async_trait::async_trait]
impl Provider for Script {
    async fn complete(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> ark_model::Result<Completion> {
        let mut text = system.to_string();
        for m in messages {
            text.push('\n');
            text.push_str(&m.content);
        }
        self.seen.lock().unwrap().push(Seen {
            text,
            tools: tools.iter().map(|t| t.name.clone()).collect(),
        });
        Ok(self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| answer("done")))
    }
    fn describe(&self) -> String {
        "m".into()
    }
}

fn answer(text: &str) -> Completion {
    Completion {
        content: text.into(),
        tool_calls: vec![],
        usage: Usage::measured(10, 2),
    }
}

fn call(name: &str, arguments: serde_json::Value) -> Completion {
    Completion {
        content: String::new(),
        tool_calls: vec![ToolCall {
            id: format!("call-{name}"),
            name: name.into(),
            arguments,
        }],
        usage: Usage::measured(10, 2),
    }
}

fn offers(seen: &Seen, action: &str) -> bool {
    seen.tools.iter().any(|t| t.contains(action)) || seen.text.contains(action)
}

async fn setup(
    replies: Vec<Completion>,
    spec: AgentSpec,
) -> (tempfile::TempDir, Arc<Script>, Orchestrator, Ark) {
    let dir = tempfile::tempdir().unwrap();
    let ark = Ark::builder()
        .store_root(dir.path())
        .provider(Arc::new(ark_model::MockProvider::new("unused", vec![])))
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.create_project("team", "a test").unwrap();
    let model = Arc::new(Script {
        replies: Mutex::new(replies.into()),
        seen: Mutex::new(Vec::new()),
    });
    let provider = model.clone();
    let o = Orchestrator::builder(&ark, &project)
        .configure_models(move |b| b.provider("m", provider as Arc<dyn Provider>))
        .build()
        .unwrap();
    o.spawn(spec).await.unwrap();
    (dir, model, o, ark)
}

#[tokio::test]
async fn a_later_chat_finds_what_an_earlier_chat_settled_without_carrying_it() {
    let (_dir, model, o, _ark) = setup(
        vec![
            answer("Agreed: the launch price is 49 euros."),
            call("conversations.search", json!({"query": "launch price"})),
            answer("We agreed on 49 euros."),
        ],
        AgentSpec::new("w").model("m"),
    )
    .await;
    let first = o.start_thread("w").unwrap();
    let t = o
        .submit(
            "w",
            TaskSpec::new("Settle the launch price: 49 euros.").thread(&first),
        )
        .unwrap();
    assert_eq!(o.wait(&t).await.unwrap().state, TaskState::Done);
    let second = o.start_thread("w").unwrap();
    let t = o
        .submit(
            "w",
            TaskSpec::new("What launch price did we settle on?").thread(&second),
        )
        .unwrap();
    let done = o.wait(&t).await.unwrap();
    assert_eq!(done.state, TaskState::Done, "{done:?}");
    let seen = model.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 3, "{seen:#?}");
    assert!(
        !offers(&seen[0], "conversations.search"),
        "a lone chat has nothing to look back at"
    );
    assert!(
        offers(&seen[1], "conversations.search"),
        "the later chat may look back"
    );
    assert!(
        !seen[1].text.contains("49 euros"),
        "the earlier chat is not in the prompt"
    );
    assert!(
        seen[2].text.contains("49 euros"),
        "the search found it: {}",
        seen[2].text
    );
    assert_eq!(done.result.as_deref(), Some("We agreed on 49 euros."));
}

#[tokio::test]
async fn two_chats_suggesting_memory_at_once_both_land() {
    let (_dir, _model, o, ark) = setup(
        vec![
            call(
                "memory.suggest",
                json!({"topic": "tone", "correction": "Write short sentences."}),
            ),
            call(
                "memory.suggest",
                json!({"topic": "units", "correction": "Use metric units."}),
            ),
        ],
        AgentSpec::new("w").model("m").allow_mutate(true),
    )
    .await;
    let (one, two) = (o.start_thread("w").unwrap(), o.start_thread("w").unwrap());
    let a = o
        .submit(
            "w",
            TaskSpec::new("Remember how I like the tone.").thread(&one),
        )
        .unwrap();
    let b = o
        .submit(
            "w",
            TaskSpec::new("Remember which units I use.").thread(&two),
        )
        .unwrap();
    for id in [&a, &b] {
        assert_eq!(o.wait(id).await.unwrap().state, TaskState::Done);
    }
    let scope = ProjectId(o.agent("w").unwrap().scope);
    let mut topics: Vec<_> = ark
        .engine()
        .store
        .project_memories(&scope)
        .unwrap()
        .into_iter()
        .map(|m| m.topic)
        .collect();
    topics.sort();
    assert_eq!(topics, ["tone", "units"]);
}

#[tokio::test]
async fn a_task_another_agent_delegated_gets_neither_memory_nor_recall() {
    let (_dir, model, o, _ark) = setup(
        vec![
            answer("Noted."),
            answer("Noted too."),
            call(
                "agents.delegate",
                json!({"to": "w", "objective": "Remember the tone for the launch.", "context": "brief"}),
            ),
        ],
        AgentSpec::new("w").model("m").allow_mutate(true),
    )
    .await;
    o.spawn(AgentSpec::new("lead").model("m").delegates_to(["w"]))
        .await
        .unwrap();
    let t = o.submit("w", TaskSpec::new("Settle the price.")).unwrap();
    assert_eq!(o.wait(&t).await.unwrap().state, TaskState::Done);
    let later = o.start_thread("w").unwrap();
    let t = o
        .submit("w", TaskSpec::new("Remember the tone.").thread(&later))
        .unwrap();
    assert_eq!(o.wait(&t).await.unwrap().state, TaskState::Done);
    let seen = model.seen.lock().unwrap().clone();
    assert!(
        offers(&seen[1], "memory.suggest") && offers(&seen[1], "conversations.search"),
        "the person's chat has both: {}",
        seen[1].text
    );
    let lead = o.submit("lead", TaskSpec::new("Coordinate.")).unwrap();
    o.wait(&lead).await.unwrap();
    let delegated = o
        .tasks()
        .into_iter()
        .find(|t| t.owner == "w" && matches!(t.origin, Origin::Agent { .. }))
        .expect("the lead delegated");
    assert_eq!(o.wait(&delegated.id).await.unwrap().state, TaskState::Done);
    let seen = model.seen.lock().unwrap().clone();
    let theirs: Vec<&Seen> = seen
        .iter()
        .filter(|s| {
            s.text.contains("Remember the tone for the launch")
                && !s.tools.iter().any(|t| t.contains("agents.delegate"))
        })
        .collect();
    assert!(!theirs.is_empty(), "{seen:#?}");
    for s in theirs {
        assert!(!offers(s, "memory.suggest"), "{:?}", s.tools);
        assert!(!offers(s, "conversations.search"), "{:?}", s.tools);
    }
}

#[tokio::test]
async fn a_chat_is_named_by_the_person_not_by_work_another_agent_handed_over() {
    let (_dir, _model, o, _ark) = setup(
        vec![
            call(
                "agents.delegate",
                json!({"to": "w", "objective": "Draft the launch post.", "context": "brief"}),
            ),
            answer("Drafted."),
            answer("Handed over."),
            answer("49 euros."),
        ],
        AgentSpec::new("w").model("m"),
    )
    .await;
    o.spawn(AgentSpec::new("lead").model("m").delegates_to(["w"]))
        .await
        .unwrap();
    let lead = o.submit("lead", TaskSpec::new("Coordinate.")).unwrap();
    o.wait(&lead).await.unwrap();
    let person = o.submit("w", TaskSpec::new("Settle the price.")).unwrap();
    assert_eq!(o.wait(&person).await.unwrap().state, TaskState::Done);
    let titles: Vec<_> = o
        .threads("w")
        .unwrap()
        .into_iter()
        .map(|t| t.title)
        .collect();
    assert_eq!(titles, ["Settle the price."]);
}
