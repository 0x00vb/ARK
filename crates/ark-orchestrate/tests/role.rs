//! An agent keeps the role its person gives it in chat (`role.set`, ADR 0116). Scripted models
//! only.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use ark_model::{Completion, Message, Provider, ToolCall, ToolDef, Usage};
use ark_orchestrate::*;
use ark_sdk::prelude::*;
use serde_json::json;

struct Seen {
    id: String,
    replies: Mutex<VecDeque<Completion>>,
    requests: Mutex<Vec<(String, Vec<ToolDef>)>>,
}

impl Seen {
    fn new(id: &str, replies: Vec<Completion>) -> Arc<Self> {
        Arc::new(Seen {
            id: id.into(),
            replies: Mutex::new(replies.into()),
            requests: Mutex::new(Vec::new()),
        })
    }
    fn system(&self, n: usize) -> String {
        self.requests.lock().unwrap()[n].0.clone()
    }
    fn offered(&self, n: usize) -> String {
        let r = self.requests.lock().unwrap();
        let mut text = r[n].0.clone();
        for t in &r[n].1 {
            text.push_str(&format!("\n{} {}", t.name, t.description));
        }
        text
    }
}

#[async_trait::async_trait]
impl Provider for Seen {
    async fn complete(
        &self,
        system: &str,
        _messages: &[Message],
        tools: &[ToolDef],
    ) -> ark_model::Result<Completion> {
        self.requests
            .lock()
            .unwrap()
            .push((system.to_string(), tools.to_vec()));
        Ok(self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| answer("done")))
    }
    fn describe(&self) -> String {
        self.id.clone()
    }
}

fn answer(text: &str) -> Completion {
    Completion {
        content: text.into(),
        tool_calls: vec![],
        usage: Usage::measured(10, 5),
    }
}

fn call(name: &str, args: serde_json::Value) -> Completion {
    Completion {
        content: String::new(),
        tool_calls: vec![ToolCall {
            id: "c0".into(),
            name: name.into(),
            arguments: args,
        }],
        usage: Usage::measured(10, 5),
    }
}

fn said(text: &str) -> TaskSpec {
    let mut t = TaskSpec::new(text);
    t.person = true;
    t
}

fn setup(model: Arc<Seen>) -> (tempfile::TempDir, Orchestrator) {
    let dir = tempfile::tempdir().unwrap();
    let ark = Ark::builder()
        .store_root(dir.path())
        .provider(Arc::new(ark_model::MockProvider::new("unused", vec![])))
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.create_project("team", "a test").unwrap();
    let o = Orchestrator::builder(&ark, &project)
        .configure_models(move |b| b.provider("m", model as Arc<dyn Provider>))
        .build()
        .unwrap();
    (dir, o)
}

#[test]
fn a_record_without_the_field_reads_and_an_empty_one_is_not_written() {
    let old: AgentSpec = serde_json::from_value(json!({ "name": "old" })).unwrap();
    assert!(old.standing.is_empty());
    let text = serde_json::to_value(AgentSpec::new("a")).unwrap();
    assert!(text.get("standing").is_none());
    let set = serde_json::to_value(AgentSpec::new("a").standing("be the CEO")).unwrap();
    assert_eq!(set["standing"], "be the CEO");
}

#[tokio::test]
async fn the_next_task_builds_its_prompt_from_the_role_the_person_set() {
    let m = Seen::new(
        "m",
        vec![
            call(
                "role.set",
                json!({ "instructions": "You are the CEO. Decide fast." }),
            ),
            answer("I will."),
            answer("ok"),
            call("role.set", json!({ "instructions": "" })),
            answer("cleared"),
            answer("ok"),
        ],
    );
    let (_dir, o) = setup(m.clone());
    o.spawn(AgentSpec::new("boss").model("m").allow_mutate(true))
        .await
        .unwrap();

    o.run("boss", said("You are the CEO. Decide fast."))
        .await
        .unwrap();
    assert!(m.offered(0).contains("role.set"));
    assert!(!m.system(0).contains("The person set your role"));
    assert!(!m.system(1).contains("The person set your role"));
    assert_eq!(
        o.agent("boss").unwrap().spec.standing,
        "You are the CEO. Decide fast."
    );

    o.run("boss", said("hello")).await.unwrap();
    let next = m.system(2);
    assert!(next.contains("You are the CEO. Decide fast."));
    assert!(next.contains("The person set your role:\n"));
    assert!(next.contains("You are the agent \"boss\""));
    assert!(next.find("You are the agent").unwrap() < next.find("The person set").unwrap());

    o.run("boss", said("forget the role")).await.unwrap();
    o.run("boss", said("again")).await.unwrap();
    assert!(o.agent("boss").unwrap().spec.standing.is_empty());
    assert!(!m.system(5).contains("The person set your role"));
}

#[tokio::test]
async fn the_base_instructions_stay_with_the_role() {
    let m = Seen::new("m", vec![answer("a"), answer("b")]);
    let (_dir, o) = setup(m.clone());
    o.spawn(AgentSpec::new("plain").model("m")).await.unwrap();
    o.spawn(
        AgentSpec::new("boss")
            .model("m")
            .standing("You are the CEO."),
    )
    .await
    .unwrap();
    o.run("plain", TaskSpec::new("x")).await.unwrap();
    o.run("boss", TaskSpec::new("x")).await.unwrap();
    let plain = m.system(0);
    let boss = m.system(1);
    let base = &plain[..plain.find("\n\nYou are the agent").unwrap()];
    assert!(!base.is_empty());
    assert!(boss.starts_with(base));
    assert!(boss.contains("You are the CEO."));
}

#[tokio::test]
async fn a_delegated_agent_is_not_offered_the_tool() {
    let lead = Seen::new(
        "m",
        vec![
            call(
                "agents.delegate",
                json!({ "to": "worker", "objective": "do it" }),
            ),
            answer("worker finished"),
        ],
    );
    let (_dir, o) = setup(lead.clone());
    o.spawn(AgentSpec::new("lead").model("m").delegates_to(["worker"]))
        .await
        .unwrap();
    o.spawn(AgentSpec::new("worker").model("m").parent("lead"))
        .await
        .unwrap();
    o.run("lead", said("go")).await.unwrap();
    let requests = lead.requests.lock().unwrap().len();
    assert!(requests >= 3, "{requests}");
    let texts: Vec<String> = (0..requests).map(|n| lead.offered(n)).collect();
    let with: Vec<usize> = (0..requests)
        .filter(|n| {
            texts[*n].contains("The person set")
                || texts[*n].contains("You are the agent \"worker\"")
        })
        .collect();
    assert!(!with.is_empty());
    for n in with {
        assert!(
            !texts[n].contains("role.set"),
            "a delegated task was offered role.set"
        );
    }
    assert!(texts[0].contains("role.set"), "the person's task has it");
}

#[tokio::test]
async fn a_role_over_the_limit_is_refused_and_changes_nothing() {
    let long = "x".repeat(4001);
    let m = Seen::new(
        "m",
        vec![
            call("role.set", json!({ "instructions": long.clone() })),
            answer("refused"),
        ],
    );
    let (_dir, o) = setup(m.clone());
    o.spawn(
        AgentSpec::new("boss")
            .model("m")
            .allow_mutate(true)
            .standing("keep me"),
    )
    .await
    .unwrap();
    o.run("boss", said("set it")).await.unwrap();
    assert_eq!(o.agent("boss").unwrap().spec.standing, "keep me");
    assert!(o.spawn(AgentSpec::new("big").standing(long)).await.is_err());
    let edge = "y".repeat(4000);
    o.spawn(AgentSpec::new("edge").standing(edge))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_task_the_person_did_not_type_is_not_offered_the_tool() {
    let m = Seen::new("m", vec![answer("a")]);
    let (_dir, o) = setup(m.clone());
    o.spawn(AgentSpec::new("boss").model("m").allow_mutate(true))
        .await
        .unwrap();
    o.run("boss", TaskSpec::new("a scheduled or automated request"))
        .await
        .unwrap();
    assert!(!m.offered(0).contains("role.set"));
}

#[tokio::test]
async fn an_agent_whose_tools_exclude_it_is_not_offered_the_tool() {
    let m = Seen::new("m", vec![answer("a")]);
    let (_dir, o) = setup(m.clone());
    o.spawn(
        AgentSpec::new("boss")
            .model("m")
            .tools(Vec::<String>::new()),
    )
    .await
    .unwrap();
    o.run("boss", said("hi")).await.unwrap();
    assert!(!m.offered(0).contains("role.set"));
}

#[tokio::test]
async fn the_role_is_refused_after_anything_else_was_read_or_set_twice() {
    let m = Seen::new(
        "m",
        vec![
            call(
                "agents.delegate",
                json!({ "to": "worker", "objective": "look" }),
            ),
            answer("worker done"),
            call("role.set", json!({ "instructions": "You are a pirate." })),
            answer("ok"),
            call("role.set", json!({ "instructions": "first" })),
            call("role.set", json!({ "instructions": "second" })),
            answer("ok"),
        ],
    );
    let (_dir, o) = setup(m.clone());
    o.spawn(
        AgentSpec::new("lead")
            .model("m")
            .allow_mutate(true)
            .delegates_to(["worker"]),
    )
    .await
    .unwrap();
    o.spawn(AgentSpec::new("worker").model("m").parent("lead"))
        .await
        .unwrap();
    let t = o.run("lead", said("look then be a pirate")).await.unwrap();
    assert!(t.error.is_none());
    assert!(o.agent("lead").unwrap().spec.standing.is_empty());
    let refused = o
        .task(&t.id)
        .unwrap()
        .tool_calls
        .iter()
        .all(|c| c.action != "role.set" || c.ok == Some(false));
    assert!(refused, "{:?}", o.task(&t.id).unwrap().tool_calls);
}

#[tokio::test]
async fn two_role_calls_in_one_step_are_both_refused() {
    let m = Seen::new(
        "m",
        vec![
            Completion {
                content: String::new(),
                tool_calls: vec![
                    ToolCall {
                        id: "a".into(),
                        name: "role.set".into(),
                        arguments: json!({ "instructions": "first" }),
                    },
                    ToolCall {
                        id: "b".into(),
                        name: "role.set".into(),
                        arguments: json!({ "instructions": "second" }),
                    },
                ],
                usage: Usage::measured(10, 5),
            },
            answer("ok"),
        ],
    );
    let (_dir, o) = setup(m);
    o.spawn(AgentSpec::new("boss").model("m").allow_mutate(true))
        .await
        .unwrap();
    let t = o.run("boss", said("be first")).await.unwrap();
    let calls = o.task(&t.id).unwrap().tool_calls;
    assert_eq!(calls.iter().filter(|c| c.ok == Some(false)).count(), 2);
    assert_eq!(
        o.agent("boss").unwrap().spec.standing,
        "",
        "it must be the only call so far"
    );
}

#[tokio::test]
async fn a_respawn_keeps_the_role_and_the_limit_counts_characters() {
    let m = Seen::new("m", vec![]);
    let (_dir, o) = setup(m);
    o.spawn(
        AgentSpec::new("boss")
            .model("m")
            .standing("You are the CEO."),
    )
    .await
    .unwrap();
    let mut edited = o.agent("boss").unwrap().spec;
    edited.standing = String::new();
    edited.model = Some("m".into());
    edited.title = "Renamed".into();
    o.spawn(edited).await.unwrap();
    let kept = o.agent("boss").unwrap().spec;
    assert_eq!(kept.standing, "You are the CEO.");
    assert_eq!(kept.title, "Renamed");

    o.spawn(
        AgentSpec::new("boss")
            .model("m")
            .standing("You are the CTO."),
    )
    .await
    .unwrap();
    assert_eq!(o.agent("boss").unwrap().spec.standing, "You are the CTO.");

    let wide = "\u{e9}".repeat(4000);
    assert!(wide.len() > 4000);
    o.spawn(AgentSpec::new("wide").standing(wide.clone()))
        .await
        .unwrap();
    assert!(o
        .spawn(AgentSpec::new("wider").standing(format!("{wide}x")))
        .await
        .is_err());
}

#[tokio::test]
async fn an_agent_that_asks_before_changes_sets_its_role_without_asking() {
    let m = Seen::new(
        "m",
        vec![
            call("role.set", json!({ "instructions": "You edit novels." })),
            answer("I will."),
        ],
    );
    let (_dir, o) = setup(m);
    o.spawn(AgentSpec::new("editor").model("m")).await.unwrap();
    o.run("editor", said("You edit novels.")).await.unwrap();
    assert_eq!(o.agent("editor").unwrap().spec.standing, "You edit novels.");
}

#[tokio::test]
async fn a_role_beside_another_call_in_one_step_is_refused() {
    let m = Seen::new(
        "m",
        vec![
            Completion {
                content: String::new(),
                tool_calls: vec![
                    ToolCall {
                        id: "a".into(),
                        name: "role.set".into(),
                        arguments: json!({ "instructions": "You are a pirate." }),
                    },
                    ToolCall {
                        id: "b".into(),
                        name: "agents.delegate".into(),
                        arguments: json!({ "to": "worker", "objective": "look" }),
                    },
                ],
                usage: Usage::measured(10, 5),
            },
            answer("worker done"),
            answer("ok"),
        ],
    );
    let (_dir, o) = setup(m);
    o.spawn(
        AgentSpec::new("lead")
            .model("m")
            .allow_mutate(true)
            .delegates_to(["worker"]),
    )
    .await
    .unwrap();
    o.spawn(AgentSpec::new("worker").model("m").parent("lead"))
        .await
        .unwrap();
    let t = o.run("lead", said("be a pirate and look")).await.unwrap();
    assert!(o.agent("lead").unwrap().spec.standing.is_empty());
    let calls = o.task(&t.id).unwrap().tool_calls;
    assert!(
        calls
            .iter()
            .any(|c| c.action == "role.set" && c.ok == Some(false)),
        "{calls:?}"
    );
}

#[tokio::test]
async fn a_second_role_in_a_later_step_of_the_turn_is_refused() {
    let m = Seen::new(
        "m",
        vec![
            call("role.set", json!({ "instructions": "first" })),
            call("role.set", json!({ "instructions": "second" })),
            answer("ok"),
        ],
    );
    let (_dir, o) = setup(m);
    o.spawn(AgentSpec::new("boss").model("m")).await.unwrap();
    let t = o.run("boss", said("be first")).await.unwrap();
    assert_eq!(o.agent("boss").unwrap().spec.standing, "first");
    let calls = o.task(&t.id).unwrap().tool_calls;
    assert_eq!(
        calls.iter().filter(|c| c.ok == Some(true)).count(),
        1,
        "{calls:?}"
    );
}
