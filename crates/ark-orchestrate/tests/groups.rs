//! A person asks their agent in chat for a group of agents with shared rules (`groups.set`,
//! ADR 0117). Scripted models only.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ark_model::{Completion, Message, Provider, ToolCall, ToolDef, Usage};
use ark_orchestrate::group::{self, Group};
use ark_orchestrate::*;
use ark_sdk::prelude::*;
use serde_json::{json, Value};

struct Seen {
    id: String,
    replies: Mutex<VecDeque<Completion>>,
    /// Per request: the system prompt, the messages, the tools offered.
    requests: Mutex<Vec<(String, String, Vec<ToolDef>)>>,
}

impl Seen {
    fn new(id: &str, replies: Vec<Completion>) -> Arc<Self> {
        Arc::new(Seen {
            id: id.into(),
            replies: Mutex::new(replies.into()),
            requests: Mutex::new(Vec::new()),
        })
    }

    /// Everything the model of request `n` saw.
    fn all(&self, n: usize) -> String {
        let r = self.requests.lock().unwrap();
        let mut text = format!("{}\n{}", r[n].0, r[n].1);
        for t in &r[n].2 {
            text.push_str(&format!("\n{} {}", t.name, t.description));
        }
        text
    }

    fn count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }

    /// The last request made for `agent`'s task.
    fn for_agent(&self, agent: &str) -> String {
        let mark = format!("You are the agent \"{agent}\"");
        (0..self.count())
            .rev()
            .map(|n| self.all(n))
            .find(|t| t.contains(&mark))
            .unwrap_or_else(|| panic!("no request for {agent}"))
    }

    /// Whether request `n` offered `name`, as a tool or in the action catalog.
    fn tool_offered(&self, n: usize, name: &str) -> bool {
        self.all(n).contains(name)
    }
}

#[async_trait::async_trait]
impl Provider for Seen {
    async fn complete(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> ark_model::Result<Completion> {
        let said = messages
            .iter()
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        self.requests
            .lock()
            .unwrap()
            .push((system.to_string(), said, tools.to_vec()));
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

fn call(name: &str, args: Value) -> Completion {
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

fn setup(model: Arc<Seen>) -> (tempfile::TempDir, Ark, Orchestrator) {
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
    (dir, ark, o)
}

async fn ceo(o: &Orchestrator) {
    let mut spec = AgentSpec::new("ceo").model("m").allow_mutate(true);
    spec.look = "owl".into();
    spec.standing = "You are the CEO.".into();
    o.spawn(spec).await.unwrap();
}

fn board() -> Value {
    json!({
        "name": "Board",
        "rules": ["Answer in Spanish.", "Never promise a date."],
        "members": [
            { "name": "cmo", "role": "Marketing", "personality": "Bold and brief." },
            { "name": "coo", "role": "Operations", "personality": "Careful and exact." }
        ]
    })
}

fn groups_in(ark: &Ark) -> Vec<Group> {
    ark.engine()
        .store
        .list_records(group::RECORD, Some(group::SCOPE))
        .unwrap()
        .into_iter()
        .map(|v| serde_json::from_value(v).unwrap())
        .collect()
}

/// A model that makes the board on the person's first message, then answers.
async fn made() -> (tempfile::TempDir, Ark, Orchestrator, Arc<Seen>, String) {
    let m = Seen::new(
        "m",
        vec![call("groups.set", board()), answer("The board is ready.")],
    );
    let (dir, ark, o) = setup(m.clone());
    ceo(&o).await;
    let t = o
        .run(
            "ceo",
            said("Make a board: a CMO and a COO. Answer in Spanish."),
        )
        .await
        .unwrap();
    assert!(t.error.is_none(), "{:?}", t.error);
    let id = groups_in(&ark)[0].id.clone();
    (dir, ark, o, m, id)
}

fn last_call(o: &Orchestrator, task: &str, action: &str) -> ToolCallRecord {
    o.task(task)
        .unwrap()
        .tool_calls
        .into_iter()
        .rfind(|c| c.action == action)
        .unwrap_or_else(|| panic!("no {action} call in {task}"))
}

async fn settled(o: &Orchestrator, agent: &str) {
    for _ in 0..500 {
        let tasks: Vec<TaskRecord> = o.tasks().into_iter().filter(|t| t.owner == agent).collect();
        if !tasks.is_empty() && tasks.iter().all(|t| t.state.is_final()) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("{agent} did not finish");
}

#[tokio::test]
async fn one_call_makes_two_agents_and_one_group() {
    let (_dir, ark, o, _m, id) = made().await;
    let groups = groups_in(&ark);
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].name, "Board");
    assert_eq!(
        groups[0].members,
        vec![
            group::Member::new(o.project().id.as_str(), "cmo"),
            group::Member::new(o.project().id.as_str(), "coo")
        ]
    );
    for (name, role, personality) in [
        ("cmo", "Marketing", "Bold and brief."),
        ("coo", "Operations", "Careful and exact."),
    ] {
        let a = o.agent(name).unwrap().spec;
        assert_eq!(a.role, role);
        assert_eq!(a.standing, personality);
        assert_eq!(a.groups, vec![id.clone()]);
        assert_eq!(a.parent.as_deref(), Some("ceo"));
        assert!(!a.allow_mutate);
        assert!(a.look.is_empty() && a.title.is_empty() && a.instructions.is_empty());
        assert!(a.delegates_to.is_empty());
    }
    assert!(o.agent("ceo").unwrap().spec.groups.is_empty());
    assert_eq!(last_call(&o, &o.tasks()[0].id, "groups.set").ok, Some(true));
    let book = ark.rule_book().get(&group::rule_set_name(&id), None);
    let set = book.unwrap().unwrap();
    assert_eq!(set.rules.len(), 2);
    assert!(set.rules.iter().all(|r| r.always));
}

#[tokio::test]
async fn a_member_task_holds_its_persona_role_personality_and_every_rule() {
    let (_dir, _ark, o, m, _id) = made().await;
    o.run("cmo", TaskSpec::new("plan the launch"))
        .await
        .unwrap();
    let seen = m.for_agent("cmo");
    assert!(
        seen.contains("You are the agent \"cmo\" (Marketing)"),
        "{seen}"
    );
    assert!(seen.contains("The person set your role:\nBold and brief."));
    assert!(seen.contains("Answer in Spanish."));
    assert!(seen.contains("Never promise a date."));
}

#[tokio::test]
async fn members_message_and_start_each_other_both_ways_but_never_delegate() {
    let (_dir, _ark, o, m, _id) = made().await;
    m.replies.lock().unwrap().extend([
        call("agents.message", json!({ "to": "coo", "text": "ready?" })),
        call(
            "agents.delegate",
            json!({ "to": "coo", "objective": "do my work" }),
        ),
        answer("cmo done"),
        call(
            "agents.start",
            json!({ "to": "cmo", "objective": "reply to me" }),
        ),
        answer("coo done"),
    ]);
    let cmo = o.run("cmo", TaskSpec::new("talk to coo")).await.unwrap();
    assert_eq!(
        last_call(&o, &cmo.id, "agents.message").ok,
        Some(true),
        "{:?}",
        o.task(&cmo.id).unwrap().tool_calls
    );
    assert!(
        o.task(&cmo.id)
            .unwrap()
            .tool_calls
            .iter()
            .all(|c| c.action != "agents.delegate" || c.ok != Some(true)),
        "a group adds no delegation"
    );
    let coo = o.run("coo", TaskSpec::new("talk to cmo")).await.unwrap();
    assert_eq!(last_call(&o, &coo.id, "agents.start").ok, Some(true));
    settled(&o, "cmo").await;
    assert!(
        o.tasks()
            .iter()
            .all(|t| !(t.owner == "coo" && t.parent.is_some())),
        "nothing was delegated"
    );
    assert!(o
        .tasks()
        .iter()
        .any(|t| t.owner == "cmo" && t.objective == "reply to me"));
}

#[tokio::test]
async fn it_is_refused_after_anything_else_ran_in_the_turn() {
    let m = Seen::new(
        "m",
        vec![
            call(
                "agents.delegate",
                json!({ "to": "worker", "objective": "look" }),
            ),
            answer("worker done"),
            call("groups.set", board()),
            answer("ok"),
        ],
    );
    let (_dir, ark, o) = setup(m);
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
    let t = o
        .run("lead", said("look, then make a board"))
        .await
        .unwrap();
    assert_eq!(last_call(&o, &t.id, "groups.set").ok, Some(false));
    assert!(o.agent("cmo").is_none() && o.agent("coo").is_none());
    assert!(groups_in(&ark).is_empty());
}

#[tokio::test]
async fn it_may_follow_role_set_but_not_run_twice() {
    let m = Seen::new(
        "m",
        vec![
            call("role.set", json!({ "instructions": "You are the CEO." })),
            call("groups.set", board()),
            call("groups.set", {
                let mut again = board();
                again["name"] = json!("Board 2");
                again
            }),
            answer("ok"),
        ],
    );
    let (_dir, ark, o) = setup(m);
    ceo(&o).await;
    let t = o
        .run("ceo", said("be the CEO and make a board"))
        .await
        .unwrap();
    let calls = o.task(&t.id).unwrap().tool_calls;
    let sets: Vec<_> = calls.iter().filter(|c| c.action == "groups.set").collect();
    assert_eq!(sets.len(), 2);
    assert_eq!(sets[0].ok, Some(true));
    assert_eq!(sets[1].ok, Some(false));
    assert_eq!(groups_in(&ark).len(), 1);
}

#[tokio::test]
async fn a_failed_attempt_does_not_use_up_the_first_call() {
    let mut bad = board();
    bad["rules"] = json!(["x".repeat(301)]);
    let m = Seen::new(
        "m",
        vec![
            call("groups.set", bad),
            call("groups.set", board()),
            answer("ok"),
        ],
    );
    let (_dir, ark, o) = setup(m);
    ceo(&o).await;
    let t = o.run("ceo", said("make a board")).await.unwrap();
    let calls = o.task(&t.id).unwrap().tool_calls;
    assert_eq!(calls[0].ok, Some(false));
    assert_eq!(calls[1].ok, Some(true), "{calls:?}");
    assert_eq!(groups_in(&ark).len(), 1);
}

#[tokio::test]
async fn it_is_offered_only_on_the_persons_own_turn() {
    let m = Seen::new(
        "m",
        vec![
            call(
                "agents.delegate",
                json!({ "to": "worker", "objective": "look" }),
            ),
            answer("worker done"),
            answer("lead done"),
            answer("scheduled"),
            answer("in a group chat"),
            answer("limited"),
        ],
    );
    let (_dir, _ark, o) = setup(m.clone());
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
    o.spawn(AgentSpec::new("limited").model("m").tools(["agents.*"]))
        .await
        .unwrap();
    o.run("lead", said("go")).await.unwrap();
    assert!(m.tool_offered(0, "groups.set"), "the person's turn has it");
    let worker = (0..m.count())
        .find(|n| m.all(*n).contains("You are the agent \"worker\""))
        .unwrap();
    assert!(
        !m.tool_offered(worker, "groups.set"),
        "a delegated task was offered groups.set"
    );
    // A scheduled run of something the person said is not the person's turn.
    let s = o
        .schedule("lead", said("a scheduled request"), When::every_minutes(10))
        .unwrap();
    let before = m.count();
    let run = o.tick_schedules(s.next_ms.unwrap()).await.remove(0);
    settled(&o, "lead").await;
    assert!(
        o.task(&run).unwrap().person,
        "the run carries the person flag"
    );
    assert!(
        !m.tool_offered(before, "groups.set"),
        "a scheduled run was offered groups.set"
    );
    let thread = o.start_thread("lead").unwrap();
    // A group chat's turn, as `group_send` submits it: a thread, not the person's flag.
    let mut group_turn = TaskSpec::new("a group chat brief");
    group_turn.thread = Some(thread);
    let before = m.count();
    o.run("lead", group_turn).await.unwrap();
    assert!(!m.tool_offered(before, "groups.set"));
    let before = m.count();
    o.run("limited", said("hi")).await.unwrap();
    assert!(!m.tool_offered(before, "groups.set"));
}

#[tokio::test]
async fn a_folder_qualified_name_is_refused_and_nothing_is_written() {
    let mut other = board();
    other["members"][0]["name"] = json!("elsewhere/cmo");
    let m = Seen::new("m", vec![call("groups.set", other), answer("refused")]);
    let (_dir, ark, o) = setup(m);
    ceo(&o).await;
    let t = o.run("ceo", said("make a board")).await.unwrap();
    assert_eq!(last_call(&o, &t.id, "groups.set").ok, Some(false));
    assert!(o.agent("coo").is_none());
    assert!(groups_in(&ark).is_empty());
}

#[tokio::test]
async fn twenty_one_rules_are_refused_and_twenty_of_300_characters_are_kept() {
    let mut many = board();
    many["rules"] = json!((0..21).map(|i| format!("rule {i}")).collect::<Vec<_>>());
    let mut edge = board();
    edge["rules"] = json!((0..20).map(|_| "y".repeat(300)).collect::<Vec<_>>());
    let m = Seen::new(
        "m",
        vec![
            call("groups.set", many),
            answer("refused"),
            call("groups.set", edge),
            answer("ok"),
        ],
    );
    let (_dir, ark, o) = setup(m);
    ceo(&o).await;
    let t = o.run("ceo", said("make a board")).await.unwrap();
    assert_eq!(last_call(&o, &t.id, "groups.set").ok, Some(false));
    assert!(groups_in(&ark).is_empty() && o.agent("cmo").is_none());
    let t = o.run("ceo", said("make it again")).await.unwrap();
    assert_eq!(last_call(&o, &t.id, "groups.set").ok, Some(true));
    let id = groups_in(&ark)[0].id.clone();
    let set = ark
        .rule_book()
        .get(&group::rule_set_name(&id), None)
        .unwrap()
        .unwrap();
    assert_eq!(set.rules.len(), 20);
}

#[tokio::test]
async fn an_invalid_second_member_leaves_nothing_written() {
    let mut half = board();
    half["members"][1] = json!({ "name": "coo" });
    let m = Seen::new("m", vec![call("groups.set", half), answer("refused")]);
    let (_dir, ark, o) = setup(m);
    ceo(&o).await;
    let t = o.run("ceo", said("make a board")).await.unwrap();
    assert_eq!(last_call(&o, &t.id, "groups.set").ok, Some(false));
    assert!(o.agent("cmo").is_none() && o.agent("coo").is_none());
    assert!(groups_in(&ark).is_empty());
    assert!(ark.rule_book().list().unwrap().is_empty());
}

#[tokio::test]
async fn an_existing_agent_joins_and_keeps_its_role_and_personality() {
    let m = Seen::new("m", vec![call("groups.set", board()), answer("ok")]);
    let (_dir, ark, o) = setup(m);
    ceo(&o).await;
    o.spawn(
        AgentSpec::new("cmo")
            .model("m")
            .role("Old role")
            .standing("Mine."),
    )
    .await
    .unwrap();
    let t = o.run("ceo", said("make a board")).await.unwrap();
    assert_eq!(last_call(&o, &t.id, "groups.set").ok, Some(true));
    let id = groups_in(&ark)[0].id.clone();
    let cmo = o.agent("cmo").unwrap().spec;
    assert_eq!(cmo.role, "Old role");
    assert_eq!(cmo.standing, "Mine.");
    assert_eq!(cmo.groups, vec![id.clone()]);
    assert!(
        o.agent("cmo").unwrap().creation_task.is_none(),
        "not made by the call"
    );
    let coo = o.agent("coo").unwrap();
    assert!(coo.creation_task.is_some());
    assert_eq!(coo.spec.groups, vec![id]);
}

#[tokio::test]
async fn a_retry_with_the_same_input_makes_no_duplicates() {
    let m = Seen::new(
        "m",
        vec![
            call("groups.set", board()),
            answer("ok"),
            call("groups.set", board()),
            answer("ok again"),
        ],
    );
    let (_dir, ark, o) = setup(m);
    ceo(&o).await;
    o.run("ceo", said("make a board")).await.unwrap();
    let t = o.run("ceo", said("make a board")).await.unwrap();
    assert_eq!(last_call(&o, &t.id, "groups.set").ok, Some(true));
    assert_eq!(groups_in(&ark).len(), 1);
    assert_eq!(o.agents().len(), 3);
    let id = groups_in(&ark)[0].id.clone();
    assert_eq!(o.agent("cmo").unwrap().spec.groups, vec![id.clone()]);
    let book = ark.rule_book();
    assert_eq!(
        book.get(&group::rule_set_name(&id), None)
            .unwrap()
            .unwrap()
            .version,
        1
    );
}

#[tokio::test]
async fn a_second_call_with_the_same_name_updates_rules_and_adds_a_member() {
    let (_dir, ark, o, m, id) = made().await;
    let update = json!({
        "name": "board",
        "rules": ["Always sign as the board."],
        "members": [{ "name": "cfo", "role": "Money", "personality": "Frugal." }]
    });
    m.replies
        .lock()
        .unwrap()
        .extend([call("groups.set", update), answer("added")]);
    let t = o
        .run("ceo", said("add a CFO; one rule only now"))
        .await
        .unwrap();
    assert_eq!(last_call(&o, &t.id, "groups.set").ok, Some(true));
    let groups = groups_in(&ark);
    assert_eq!(groups.len(), 1, "same name, same group");
    assert_eq!(groups[0].id, id);
    let names: Vec<&str> = groups[0].members.iter().map(|m| m.agent.as_str()).collect();
    assert_eq!(
        names,
        ["cmo", "coo", "cfo"],
        "members absent from the call stay"
    );
    for name in ["cmo", "coo", "cfo"] {
        assert_eq!(o.agent(name).unwrap().spec.groups, vec![id.clone()]);
    }
    assert_eq!(o.agent("cfo").unwrap().spec.standing, "Frugal.");
    let set = ark
        .rule_book()
        .get(&group::rule_set_name(&id), None)
        .unwrap()
        .unwrap();
    assert_eq!(set.version, 2);
    assert_eq!(set.rules.len(), 1);
    assert_eq!(o.agents().len(), 4);
}

#[tokio::test]
async fn a_call_without_rules_keeps_them_and_equal_rules_make_no_new_version() {
    let (_dir, ark, o, m, id) = made().await;
    let mut no_rules = board();
    no_rules.as_object_mut().unwrap().remove("rules");
    no_rules["members"] = json!([{ "name": "cfo", "role": "Money" }]);
    m.replies.lock().unwrap().extend([
        call("groups.set", no_rules),
        answer("added"),
        call("groups.set", board()),
        answer("again"),
    ]);
    o.run("ceo", said("add a CFO")).await.unwrap();
    o.run("ceo", said("same rules as before")).await.unwrap();
    let set = ark
        .rule_book()
        .get(&group::rule_set_name(&id), None)
        .unwrap()
        .unwrap();
    assert_eq!(set.version, 1);
    assert_eq!(set.rules.len(), 2);
    assert_eq!(groups_in(&ark)[0].members.len(), 3);
}

#[tokio::test]
async fn new_rules_reach_the_next_task_and_scheduled_runs() {
    let (_dir, ark, o, m, id) = made().await;
    let mut update = board();
    update["rules"] = json!(["Always sign as the board."]);
    update["members"] = json!([{ "name": "coo" }, { "name": "cmo" }]);
    m.replies
        .lock()
        .unwrap()
        .extend([call("groups.set", update), answer("rules changed")]);
    let t = o.run("ceo", said("change the board's rule")).await.unwrap();
    assert_eq!(last_call(&o, &t.id, "groups.set").ok, Some(true));
    let version = ark
        .rule_book()
        .get(&group::rule_set_name(&id), None)
        .unwrap()
        .unwrap()
        .version;
    assert_eq!(version, 2);
    o.run("coo", TaskSpec::new("a scheduled report"))
        .await
        .unwrap();
    let seen = m.for_agent("coo");
    assert!(seen.contains("Always sign as the board."), "{seen}");
    assert!(
        !seen.contains("Answer in Spanish."),
        "the new version replaces the old"
    );
}

#[tokio::test]
async fn twenty_always_rules_per_agent_across_all_its_groups() {
    let rules = |n: usize| json!((0..n).map(|i| format!("rule {i}")).collect::<Vec<_>>());
    let mut first = board();
    first["rules"] = rules(12);
    let mut second = json!({
        "name": "Ops",
        "rules": rules(12),
        "members": [
            { "name": "cmo" },
            { "name": "finance", "role": "Money" }
        ]
    });
    let m = Seen::new(
        "m",
        vec![
            call("groups.set", first),
            answer("ok"),
            call("groups.set", second.clone()),
            answer("refused"),
            call("groups.set", {
                second["rules"] = rules(8);
                second.clone()
            }),
            answer("ok"),
        ],
    );
    let (_dir, ark, o) = setup(m);
    ceo(&o).await;
    o.run("ceo", said("make a board")).await.unwrap();
    let t = o.run("ceo", said("make ops")).await.unwrap();
    assert_eq!(last_call(&o, &t.id, "groups.set").ok, Some(false));
    assert!(o.agent("finance").is_none());
    assert_eq!(groups_in(&ark).len(), 1);
    let t = o
        .run("ceo", said("make ops with fewer rules"))
        .await
        .unwrap();
    assert_eq!(last_call(&o, &t.id, "groups.set").ok, Some(true));
    assert_eq!(o.agent("cmo").unwrap().spec.groups.len(), 2);
}

#[tokio::test]
async fn removing_a_member_ends_its_contact_and_its_rules() {
    let (_dir, _ark, o, m, id) = made().await;
    o.leave_group(&id, &["cmo".to_string(), "nobody".to_string()])
        .unwrap();
    assert!(o.agent("cmo").unwrap().spec.groups.is_empty());
    assert_eq!(o.agent("coo").unwrap().spec.groups, vec![id]);
    m.replies.lock().unwrap().extend([
        call(
            "agents.message",
            json!({ "to": "coo", "text": "still there?" }),
        ),
        answer("cmo done"),
        call(
            "agents.message",
            json!({ "to": "cmo", "text": "still there?" }),
        ),
        answer("coo done"),
    ]);
    let cmo = o.run("cmo", TaskSpec::new("talk")).await.unwrap();
    assert_eq!(last_call(&o, &cmo.id, "agents.message").ok, Some(false));
    let coo = o.run("coo", TaskSpec::new("talk")).await.unwrap();
    assert_eq!(last_call(&o, &coo.id, "agents.message").ok, Some(false));
    assert!(
        !m.for_agent("cmo").contains("Never promise a date."),
        "its rules ended with its membership"
    );
    assert!(m.for_agent("coo").contains("Never promise a date."));
}

#[tokio::test]
async fn agents_create_no_longer_copies_look_or_standing() {
    let m = Seen::new(
        "m",
        vec![
            call(
                "agents.create",
                json!({ "name": "helper", "role": "Helps", "instructions": "Be brief." }),
            ),
            answer("ok"),
        ],
    );
    let (_dir, _ark, o) = setup(m);
    let mut spec = AgentSpec::new("ceo")
        .model("m")
        .allow_mutate(true)
        .skills(["writing"])
        .budget(Budget::default().steps(7))
        .delegates_to(["helper"]);
    spec.look = "owl".into();
    spec.title = "The CEO".into();
    spec.standing = "You are the CEO.".into();
    spec.groups = vec!["g-old".into()];
    o.spawn(spec).await.unwrap();
    o.run("ceo", said("create a helper")).await.unwrap();
    let h = o.agent("helper").unwrap().spec;
    assert!(h.look.is_empty() && h.title.is_empty() && h.standing.is_empty());
    assert!(h.delegates_to.is_empty() && h.groups.is_empty());
    assert_eq!(h.instructions, "Be brief.");
    assert_eq!(h.role, "Helps");
    assert_eq!(h.parent.as_deref(), Some("ceo"));
    assert!(!h.allow_mutate);
    assert_eq!(h.skills, vec!["writing".to_string()]);
    assert_eq!(h.budget.max_steps, Some(7));
    assert_eq!(h.tools, vec!["*".to_string()]);
}

#[test]
fn what_v0_45_stored_still_reads() {
    let old: Group = serde_json::from_value(json!({
        "id": "g-1", "name": "Pair",
        "members": [{ "project": "p", "agent": "a" }],
        "created_ms": 1, "updated_ms": 2
    }))
    .unwrap();
    assert_eq!(old.members, vec![group::Member::new("p", "a")]);
    assert!(old.threads.is_empty() && old.cursors.is_empty());
    let spec: AgentSpec = serde_json::from_value(json!({ "name": "old" })).unwrap();
    assert!(spec.groups.is_empty());
    assert!(serde_json::to_value(AgentSpec::new("a"))
        .unwrap()
        .get("groups")
        .is_none());
    let mut grouped = AgentSpec::new("a");
    grouped.groups = vec!["g-1".into()];
    assert_eq!(
        serde_json::to_value(grouped).unwrap()["groups"],
        json!(["g-1"])
    );
}

#[tokio::test]
async fn a_spawn_over_a_member_with_an_older_spec_keeps_its_groups() {
    let (_dir, _ark, o, _m, id) = made().await;
    o.spawn(AgentSpec::new("cmo").model("m").role("Marketing"))
        .await
        .unwrap();
    assert_eq!(o.agent("cmo").unwrap().spec.groups, vec![id]);
}

#[tokio::test]
async fn members_of_a_window_group_join_when_a_chat_sets_it() {
    let m = Seen::new(
        "m",
        vec![
            call(
                "groups.set",
                json!({ "name": "Pair", "rules": ["Answer in Spanish."], "members": [
                    { "name": "cmo", "role": "Marketing" } ] }),
            ),
            answer("done"),
        ],
    );
    let (_dir, ark, o) = setup(m);
    ceo(&o).await;
    o.spawn(AgentSpec::new("old").model("m")).await.unwrap();
    let project = o.project().id.as_str().to_string();
    let window = Group {
        id: "g-window".into(),
        name: "Pair".into(),
        members: vec![group::Member::new(&project, "old")],
        threads: Default::default(),
        cursors: Default::default(),
        created_ms: 1,
        updated_ms: 1,
    };
    ark.engine()
        .store
        .put_record(
            group::RECORD,
            &window.id,
            group::SCOPE,
            &serde_json::to_value(&window).unwrap(),
        )
        .unwrap();
    let t = o.run("ceo", said("put a cmo in Pair")).await.unwrap();
    assert_eq!(last_call(&o, &t.id, "groups.set").ok, Some(true));
    assert_eq!(
        o.agent("old").unwrap().spec.groups,
        vec!["g-window".to_string()]
    );
    assert_eq!(
        o.agent("cmo").unwrap().spec.groups,
        vec!["g-window".to_string()]
    );
    assert_eq!(groups_in(&ark)[0].members.len(), 2);
}

/// Answers each agent from its own list, so agents that run at once get their own replies.
struct ByAgent(
    Mutex<std::collections::HashMap<String, VecDeque<Completion>>>,
    /// The system prompt of every request, in order.
    Mutex<Vec<String>>,
);

impl ByAgent {
    fn new(script: &[(&str, Vec<Completion>)]) -> Arc<Self> {
        Arc::new(ByAgent(
            Mutex::new(
                script
                    .iter()
                    .map(|(a, r)| (a.to_string(), r.clone().into()))
                    .collect(),
            ),
            Mutex::new(Vec::new()),
        ))
    }
}

#[async_trait::async_trait]
impl Provider for ByAgent {
    async fn complete(
        &self,
        system: &str,
        messages: &[Message],
        _tools: &[ToolDef],
    ) -> ark_model::Result<Completion> {
        let text = format!(
            "{system}\n{}",
            messages
                .iter()
                .map(|m| m.content.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        );
        self.1.lock().unwrap().push(system.to_string());
        let mut all = self.0.lock().unwrap();
        let reply = all
            .iter_mut()
            .find(|(a, _)| text.contains(&format!("You are the agent \"{a}\"")))
            .and_then(|(_, r)| r.pop_front());
        Ok(reply.unwrap_or_else(|| answer("done")))
    }

    fn describe(&self) -> String {
        "m".into()
    }
}

/// A state-changing tool that counts what it really changed.
struct Doc(Arc<std::sync::atomic::AtomicUsize>);

#[async_trait::async_trait]
impl Adapter for Doc {
    fn name(&self) -> &str {
        "doc"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![ActionDescriptor::new("doc.save", "save", RiskTier::Mutate)]
    }
    async fn run(&self, _: &str, input: Value, _: &RunCtx<'_>) -> ark_tools::Result<ActionOutput> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(ActionOutput::json(json!({ "saved": input["text"] })))
    }
}

/// Says no to every question, counting them.
struct Refuse(Arc<std::sync::atomic::AtomicUsize>);

#[async_trait::async_trait]
impl Approver for Refuse {
    async fn decide(&self, _: &ApprovalRequest) -> ApprovalDecision {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        ApprovalDecision::Deny
    }
}

#[tokio::test]
async fn work_a_member_starts_through_the_group_asks_even_on_an_agent_that_may_act_alone() {
    let ops = through_the_group(false).await;
    let (asks, free) = (
        "Some changes ask for approval first",
        "You make changes without asking.",
    );
    assert!(
        !ops[0].contains(asks) && !ops[0].contains(free),
        "work through the group has no one to ask here, so its prompt names no authority"
    );
    let last = ops.last().unwrap();
    assert!(
        last.contains(free) && !last.contains(asks),
        "the person's own request does not"
    );
}

#[tokio::test]
async fn work_through_the_group_with_a_gate_is_told_it_asks_and_does_ask() {
    let ops = through_the_group(true).await;
    let (asks, free) = (
        "Some changes ask for approval first",
        "You make changes without asking.",
    );
    assert!(
        ops[0].contains(asks) && !ops[0].contains(free),
        "work through the group asks, so its prompt says so"
    );
    let last = ops.last().unwrap();
    assert!(last.contains(free) && !last.contains(asks));
}

/// The system prompts of `ops` (an agent that may act alone) when `cmo` hands it work through a
/// group and then the person asks it directly. With a gate, the group's work is asked about
/// once and refused; the person's own request still runs.
async fn through_the_group(gate: bool) -> Vec<String> {
    let saves = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let m = ByAgent::new(&[
        (
            "ceo",
            vec![
                call(
                    "groups.set",
                    json!({ "name": "Ops", "members": [
                        { "name": "ops" }, { "name": "cmo", "role": "Marketing" } ] }),
                ),
                answer("ready"),
            ],
        ),
        (
            "cmo",
            vec![
                call("agents.message", json!({ "to": "ops", "text": "save a" })),
                call(
                    "agents.start",
                    json!({ "to": "ops", "objective": "save a" }),
                ),
                answer("asked"),
            ],
        ),
        (
            "ops",
            vec![
                call("doc.save", json!({ "text": "a" })),
                answer("tried"),
                call("doc.save", json!({ "text": "b" })),
                answer("saved"),
            ],
        ),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let ark = Ark::builder()
        .store_root(dir.path())
        .tool(Arc::new(Doc(saves.clone())))
        .provider(Arc::new(ark_model::MockProvider::new("unused", vec![])))
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.create_project("team", "a test").unwrap();
    let mm = m.clone();
    let mut builder = Orchestrator::builder(&ark, &project)
        .configure_models(move |b| b.provider("m", mm as Arc<dyn Provider>));
    if gate {
        builder = builder.approvals(Arc::new(ApprovalGate::new(Arc::new(Refuse(asked.clone())))));
    }
    let o = builder.build().unwrap();
    ceo(&o).await;
    o.spawn(AgentSpec::new("ops").model("m").allow_mutate(true))
        .await
        .unwrap();
    let t = o.run("ceo", said("make Ops with a cmo")).await.unwrap();
    assert_eq!(last_call(&o, &t.id, "groups.set").ok, Some(true));
    let cmo = o
        .run("cmo", TaskSpec::new("get ops to save"))
        .await
        .unwrap();
    assert_eq!(
        last_call(&o, &cmo.id, "agents.message").ok,
        Some(false),
        "a group member's text could steer an agent that does not ask"
    );
    settled(&o, "ops").await;
    let started = o
        .tasks()
        .into_iter()
        .find(|t| t.owner == "ops")
        .expect("cmo started work on ops");
    assert_ne!(last_call(&o, &started.id, "doc.save").ok, Some(true));
    assert_eq!(saves.load(std::sync::atomic::Ordering::SeqCst), 0);
    o.run("ops", said("save b")).await.unwrap();
    assert_eq!(
        saves.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the person's own request still runs without asking"
    );
    if gate {
        assert_eq!(
            asked.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the group's work asked once"
        );
    }
    let ops =
        m.1.lock()
            .unwrap()
            .iter()
            .filter(|s| s.contains("You are the agent \"ops\""))
            .cloned()
            .collect();
    ops
}

#[tokio::test]
async fn a_member_that_cannot_be_made_ends_those_made_and_a_retry_finishes() {
    let m = Seen::new(
        "m",
        vec![
            call("groups.set", board()),
            answer("failed"),
            call("groups.set", board()),
            answer("done"),
        ],
    );
    let (_dir, ark, o) = setup(m);
    ceo(&o).await;
    // `coo` has work in another live runtime, so it cannot be made here.
    let mut other = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let held = json!({
        "id": "t-held", "project": o.project().id.as_str(), "owner": "coo",
        "objective": "held", "origin": { "by": "external" }, "state": "running",
        "attempts": 1, "created_ms": 1, "started_ms": 1, "submission": "s-elsewhere",
        "execution_owner": "elsewhere", "pid": other.id(),
    });
    ark.engine()
        .store
        .put_record("orch_task", "t-held", o.project().id.as_str(), &held)
        .unwrap();
    let t = o.run("ceo", said("make a board")).await.unwrap();
    let _ = other.kill();
    let _ = other.wait();
    assert_eq!(last_call(&o, &t.id, "groups.set").ok, Some(false));
    assert_eq!(o.agent("cmo").unwrap().state, AgentState::Terminated);
    assert!(o.agent("coo").is_none());
    assert!(groups_in(&ark).is_empty());
    assert!(ark.rule_book().list().unwrap().is_empty());
    let t = o.run("ceo", said("make a board")).await.unwrap();
    assert_eq!(last_call(&o, &t.id, "groups.set").ok, Some(true));
    for name in ["cmo", "coo"] {
        let a = o.agent(name).unwrap();
        assert_ne!(a.state, AgentState::Terminated);
        assert_eq!(a.spec.groups.len(), 1);
    }
    assert_eq!(groups_in(&ark).len(), 1);
}

#[tokio::test]
async fn a_member_the_person_ended_is_not_made_again_by_the_same_call() {
    let (_dir, _ark, o, m, _id) = made().await;
    o.terminate("coo").await.unwrap();
    m.replies
        .lock()
        .unwrap()
        .extend([call("groups.set", board()), answer("refused")]);
    let t = o.run("ceo", said("make the board again")).await.unwrap();
    assert_eq!(last_call(&o, &t.id, "groups.set").ok, Some(false));
    assert_eq!(o.agent("coo").unwrap().state, AgentState::Terminated);
}

#[tokio::test]
async fn work_a_member_started_still_asks_after_it_is_delegated_on() {
    let saves = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let m = ByAgent::new(&[
        (
            "ceo",
            vec![
                call(
                    "groups.set",
                    json!({ "name": "Ops", "members": [
                        { "name": "lead" }, { "name": "cmo", "role": "Marketing" } ] }),
                ),
                answer("ready"),
            ],
        ),
        (
            "cmo",
            vec![
                call(
                    "agents.start",
                    json!({ "to": "lead", "objective": "have bob save" }),
                ),
                answer("asked"),
            ],
        ),
        (
            "lead",
            vec![
                call(
                    "agents.delegate",
                    json!({ "to": "bob", "objective": "save c" }),
                ),
                answer("handed on"),
            ],
        ),
        (
            "bob",
            vec![call("doc.save", json!({ "text": "c" })), answer("tried")],
        ),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let ark = Ark::builder()
        .store_root(dir.path())
        .tool(Arc::new(Doc(saves.clone())))
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
    ceo(&o).await;
    o.spawn(AgentSpec::new("bob").model("m").allow_mutate(true))
        .await
        .unwrap();
    o.spawn(AgentSpec::new("lead").model("m").delegates_to(["bob"]))
        .await
        .unwrap();
    let t = o.run("ceo", said("make Ops with a cmo")).await.unwrap();
    assert_eq!(last_call(&o, &t.id, "groups.set").ok, Some(true));
    o.run("cmo", TaskSpec::new("get it saved")).await.unwrap();
    settled(&o, "lead").await;
    settled(&o, "bob").await;
    assert!(
        o.tasks().iter().any(|t| t.owner == "bob"),
        "lead handed the work on"
    );
    assert_eq!(saves.load(std::sync::atomic::Ordering::SeqCst), 0);
}
