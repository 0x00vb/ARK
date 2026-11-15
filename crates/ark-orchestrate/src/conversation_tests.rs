use super::*;

fn fixture() -> (tempfile::TempDir, Ark, Project) {
    let dir = tempfile::tempdir().unwrap();
    let ark = Ark::builder()
        .store_root(dir.path())
        .provider(Arc::new(ark_model::MockProvider::new("test", vec![])))
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.create_project("team", "test").unwrap();
    (dir, ark, project)
}

impl Orchestrator {
    pub(super) async fn admit_confirmed_test_run(
        &self,
        automation: &str,
        expected: u64,
        origin: crate::automation::RunOrigin,
        inputs: std::collections::BTreeMap<String, crate::automation::InputValue>,
    ) -> Result<crate::automation::Run, OrchError> {
        if let crate::automation::RunOrigin::Manual { request } = &origin {
            let previous = self
                .automation(automation)?
                .last_start
                .map(|receipt| receipt.run);
            let start = crate::automation::ManualStart {
                automation: automation.into(),
                expected,
                request: request.clone(),
                inputs,
                previous,
            };
            let run = self.admit_manual_automation_run(start.clone()).await?;
            self.acknowledge_automation_start(start)?;
            Ok(run)
        } else {
            self.admit_automation_run(automation, expected, origin, inputs)
                .await
        }
    }
}

fn test_models() -> Arc<dyn ModelResolver> {
    let mut builder = crate::Models::builder().default_model("test");
    for spec in [
        "test",
        "provider-a~one:large@high",
        "provider-a~one:large@low",
        "provider-b:small",
    ] {
        builder = builder.provider(spec, Arc::new(ark_model::MockProvider::new(spec, vec![])));
    }
    Arc::new(builder.build())
}

async fn source(o: &Orchestrator, tools: &[&str], budget: Option<crate::Budget>) -> String {
    o.spawn(AgentSpec::new("source").tools(tools.iter().copied()))
        .await
        .unwrap();
    let mut spec = TaskSpec::new("external request");
    spec.budget = budget;
    o.inner
        .create_task("source", spec, Origin::External, None)
        .unwrap()
}

#[tokio::test]
async fn conversation_creation_is_serialized_and_keeps_permissions() {
    let (_dir, ark, project) = fixture();
    let o = Orchestrator::builder(&ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    let task = source(&o, &["agents.create", "files.read"], None).await;
    let (a, b) = tokio::join!(
        o.inner.create_colleague(
            "source",
            &task,
            "colleague",
            "review",
            "read carefully",
            Default::default()
        ),
        o.inner.create_colleague(
            "source",
            &task,
            "colleague",
            "review",
            "read carefully",
            Default::default()
        )
    );
    let a = a.unwrap();
    assert_eq!(a.scope, b.unwrap().scope);
    assert_eq!(o.agents().len(), 2);
    assert_eq!(a.spec.tools, vec!["agents.create", "files.read"]);
    assert!(!a.spec.allow_mutate);
    assert!(a.spec.delegates_to.is_empty());
    assert_eq!(a.creation_root.as_deref(), Some(task.as_str()));
    assert!(o.inner.contact_allowed("source", "colleague"));
    assert!(o.inner.contact_allowed("colleague", "source"));
    assert!(o.tasks().iter().all(|t| t.owner != "colleague"));
    let another = o
        .inner
        .create_task(
            "source",
            TaskSpec::new("another request"),
            Origin::External,
            None,
        )
        .unwrap();
    assert!(o
        .inner
        .create_colleague(
            "source",
            &another,
            "colleague",
            "replace",
            "overwrite",
            Default::default()
        )
        .await
        .is_err());
    assert_eq!(o.agent("colleague").unwrap().spec.role, "review");
}

#[tokio::test]
async fn conversation_creation_respects_allowlists_and_root_quota_after_reload() {
    let (_dir, ark, project) = fixture();
    let o = Orchestrator::builder(&ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    let task = source(&o, &["files.read"], None).await;
    assert!(o
        .inner
        .create_colleague(
            "source",
            &task,
            "forbidden",
            "review",
            "",
            Default::default()
        )
        .await
        .is_err());
    o.spawn(AgentSpec::new("source")).await.unwrap();
    let task = o
        .inner
        .create_task(
            "source",
            TaskSpec::new("limited").budget(crate::Budget::default().delegations(1)),
            Origin::External,
            None,
        )
        .unwrap();
    let a = o
        .inner
        .create_colleague("source", &task, "first", "review", "", Default::default())
        .await
        .unwrap();
    let descendant = o
        .inner
        .create_task(
            "first",
            TaskSpec::new("descendant"),
            Origin::Agent {
                agent: "source".into(),
                task: task.clone(),
            },
            None,
        )
        .unwrap();
    assert!(o
        .inner
        .create_colleague(
            "first",
            &descendant,
            "second",
            "review",
            "",
            Default::default()
        )
        .await
        .is_err());
    assert_eq!(
        o.inner
            .create_colleague("source", &task, "first", "review", "", Default::default())
            .await
            .unwrap()
            .scope,
        a.scope
    );
    let reloaded = Orchestrator::builder(&ark, &project).build().unwrap();
    assert_eq!(reloaded.agent("first").unwrap().scope, a.scope);
    assert!(reloaded
        .inner
        .create_colleague("source", &task, "second", "review", "", Default::default())
        .await
        .is_err());
}

#[tokio::test]
async fn conversation_start_is_independent_idempotent_and_scoped() {
    let (_dir, ark, project) = fixture();
    let o = Orchestrator::builder(&ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    let task = source(&o, &["*"], None).await;
    o.inner
        .create_colleague(
            "source",
            &task,
            "colleague",
            "review",
            "",
            Default::default(),
        )
        .await
        .unwrap();
    let spec = TaskSpec::new("first brief").context("necessary context");
    let (a, b) = tokio::join!(
        o.inner
            .start_conversation("source", &task, "colleague", spec.clone()),
        o.inner
            .start_conversation("source", &task, "colleague", spec.clone())
    );
    let id = a.unwrap();
    assert_eq!(id, b.unwrap());
    assert!(o.task(&id).unwrap().parent.is_none());
    assert!(o.task(&task).unwrap().children.is_empty());
    assert!(
        matches!(o.task(&id).unwrap().origin, Origin::Agent { agent, task: origin } if agent == "source" && origin == task)
    );
    o.spawn(AgentSpec::new("stranger")).await.unwrap();
    assert!(o
        .inner
        .start_conversation("source", &task, "stranger", spec)
        .await
        .is_err());
    o.cancel(&task).unwrap();
    assert_ne!(o.wait(&id).await.unwrap().state, TaskState::Cancelled);
    let reloaded = Orchestrator::builder(&ark, &project).build().unwrap();
    assert_eq!(
        reloaded
            .inner
            .start_conversation(
                "source",
                &task,
                "colleague",
                TaskSpec::new("first brief").context("necessary context")
            )
            .await
            .unwrap(),
        id
    );
}

#[tokio::test]
async fn conversation_start_enforces_the_external_root_limit_on_descendants() {
    let (_dir, ark, project) = fixture();
    let o = Orchestrator::builder(&ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    let task = source(&o, &["*"], Some(crate::Budget::default().delegations(1))).await;
    o.inner
        .create_colleague(
            "source",
            &task,
            "colleague",
            "review",
            "",
            Default::default(),
        )
        .await
        .unwrap();
    let id = o
        .inner
        .start_conversation("source", &task, "colleague", TaskSpec::new("first"))
        .await
        .unwrap();
    let descendant = o
        .inner
        .create_task(
            "colleague",
            TaskSpec::new("followup"),
            Origin::Agent {
                agent: "source".into(),
                task: id.clone(),
            },
            None,
        )
        .unwrap();
    assert!(o
        .inner
        .start_conversation("colleague", &descendant, "source", TaskSpec::new("loop"))
        .await
        .is_err());
    o.wait(&id).await.unwrap();
    let fresh = o
        .inner
        .create_task("source", TaskSpec::new("tomorrow"), Origin::External, None)
        .unwrap();
    assert!(o
        .inner
        .start_conversation("source", &fresh, "colleague", TaskSpec::new("new day"))
        .await
        .is_ok());
}

struct DelayedModels {
    base: Arc<dyn ModelResolver>,
    entered: tokio::sync::Notify,
    released: tokio::sync::Notify,
    closed: std::sync::atomic::AtomicBool,
}

#[async_trait::async_trait]
impl ModelResolver for DelayedModels {
    async fn resolve(&self, lane: &str, spec: Option<&str>) -> Result<AgentModels, OrchError> {
        if lane.ends_with("/colleague") {
            self.entered.notify_one();
            self.released.notified().await;
        }
        self.base.resolve(lane, spec).await
    }
    async fn close(&self, lane: &str) {
        if lane.ends_with("/colleague") {
            self.closed.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        self.base.close(lane).await;
    }
}

#[tokio::test]
async fn conversation_creation_cannot_publish_after_cancellation_during_model_resolution() {
    let (_dir, ark, project) = fixture();
    let base = Orchestrator::builder(&ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    let models = Arc::new(DelayedModels {
        base: base.models().clone(),
        entered: tokio::sync::Notify::new(),
        released: tokio::sync::Notify::new(),
        closed: std::sync::atomic::AtomicBool::new(false),
    });
    let o = Orchestrator::builder(&ark, &project)
        .models(models.clone())
        .build()
        .unwrap();
    let task = source(&o, &["*"], None).await;
    let creating = {
        let o = o.clone();
        let task = task.clone();
        tokio::spawn(async move {
            o.inner
                .create_colleague(
                    "source",
                    &task,
                    "colleague",
                    "review",
                    "",
                    Default::default(),
                )
                .await
        })
    };
    models.entered.notified().await;
    o.cancel(&task).unwrap();
    models.released.notify_one();
    assert!(creating.await.unwrap().is_err());
    assert!(o.agent("colleague").is_none());
    assert!(
        crate::agent_contacts(&ark.engine().store, project.id.as_str())
            .unwrap()
            .iter()
            .all(|a| a.name != "colleague")
    );
    assert!(models.closed.load(std::sync::atomic::Ordering::SeqCst));
}

#[tokio::test]
async fn a_turn_that_was_stopped_hands_off_no_work() {
    let (_dir, ark, project) = fixture();
    let o = Orchestrator::builder(&ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    o.spawn(AgentSpec::new("source")).await.unwrap();
    let mut spec = TaskSpec::new("look into it");
    spec.person = true;
    let turn = o
        .inner
        .create_task("source", spec, Origin::External, None)
        .unwrap();
    o.cancel(&turn).unwrap();
    let refused = o
        .inner
        .work_later("source", &turn, "read the filings")
        .await;
    assert!(refused.is_err(), "a stopped turn handed work off");
    assert!(
        o.tasks().iter().all(|t| !t.reply),
        "work was made for a stopped turn"
    );
}

#[tokio::test]
async fn task_and_work_item_commit_together() {
    let (dir, ark, project) = fixture();
    let o = Orchestrator::builder(&ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    o.spawn(AgentSpec::new("reader")).await.unwrap();
    let item = o
        .add_work(crate::board::PERSON, "work", crate::WorkChange::default())
        .unwrap();
    let db = rusqlite::Connection::open(dir.path().join("meta.db")).unwrap();
    db.execute_batch("CREATE TRIGGER reject_work BEFORE INSERT ON records WHEN NEW.kind = 'orch_work' BEGIN SELECT RAISE(ABORT, 'injected failure'); END;").unwrap();
    assert!(o
        .inner
        .create_task(
            "reader",
            TaskSpec::new("do work").item(&item.id),
            Origin::External,
            None
        )
        .is_err());
    assert!(o.tasks().is_empty());
    assert!(o.work_item(&item.id).unwrap().tasks.is_empty());
    assert!(ark
        .engine()
        .store
        .list_records(TASK, Some(project.id.as_str()))
        .unwrap()
        .is_empty());
    db.execute_batch("DROP TRIGGER reject_work;").unwrap();
    let task = o
        .inner
        .create_task(
            "reader",
            TaskSpec::new("do work").item(&item.id),
            Origin::External,
            None,
        )
        .unwrap();
    let reloaded = Orchestrator::builder(&ark, &project).build().unwrap();
    assert_eq!(
        reloaded.work_item(&item.id).unwrap().tasks,
        vec![task.clone()]
    );
    assert_eq!(
        reloaded.task(&task).unwrap().item.as_deref(),
        Some(item.id.as_str())
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refused_concurrent_board_claims_leave_no_durable_tasks_to_recover() {
    let (_dir, ark, project) = fixture();
    let o = Orchestrator::builder(&ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    o.spawn(AgentSpec::new("first")).await.unwrap();
    o.spawn(AgentSpec::new("second")).await.unwrap();
    for index in 0..16 {
        let item = o
            .add_work(
                crate::board::PERSON,
                &format!("work {index}"),
                crate::WorkChange::default(),
            )
            .unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let run = |name: &'static str| {
            let o = o.clone();
            let item = item.id.clone();
            let barrier = barrier.clone();
            tokio::task::spawn_blocking(move || {
                barrier.wait();
                o.inner.create_task(
                    name,
                    TaskSpec::new("claim this item").item(item),
                    Origin::External,
                    None,
                )
            })
        };
        let a = run("first");
        let b = run("second");
        let (a, b) = tokio::join!(a, b);
        assert_eq!(
            usize::from(a.unwrap().is_ok()) + usize::from(b.unwrap().is_ok()),
            1
        );
    }
    assert_eq!(o.tasks().len(), 16);
    assert_eq!(
        ark.engine()
            .store
            .list_records(TASK, Some(project.id.as_str()))
            .unwrap()
            .len(),
        16
    );
    let reloaded = Orchestrator::builder(&ark, &project).build().unwrap();
    assert_eq!(reloaded.tasks().len(), 16);
}

struct LookupActions;

#[async_trait::async_trait]
impl ark_tools::Adapter for LookupActions {
    fn name(&self) -> &str {
        "lookups"
    }
    fn actions(&self) -> Vec<ark_core::ActionDescriptor> {
        [
            (
                "crm.find_customer",
                "find a customer by name; returns id, name and tier",
            ),
            ("crm.orders", "list a customer's orders with amounts"),
            (
                "weather.current",
                "current temperature for a city (slow service)",
            ),
        ]
        .into_iter()
        .map(|(name, summary)| {
            ark_core::ActionDescriptor::new(name, summary, ark_core::RiskTier::Read)
        })
        .collect()
    }
    async fn run(
        &self,
        action: &str,
        _: serde_json::Value,
        _: &ark_tools::RunCtx<'_>,
    ) -> ark_tools::Result<ark_tools::ActionOutput> {
        Err(ark_tools::AdapterError::UnknownAction(action.into()))
    }
}

#[test]
fn conversation_discovery_does_not_displace_the_actions_needed_by_a_work_brief() {
    let mut registry = ark_tools::Registry::new();
    registry.register(Arc::new(LookupActions));
    registry.register(Arc::new(AgentTools {
        orch: Weak::new(),
        agent: "reader".into(),
        task: "t-reader".into(),
        delegates: Vec::new(),
        contacts: vec!["lead".into()],
        can_create: false,
        schedule: None,
    }));
    let weather = ark_runtime::catalog::matching_actions(
        &registry,
        "Task t-weather delegated by lead (from its task t-lead).\n\n== OBJECTIVE ==\nGet the current temperature in Paris and in Tokyo.",
        2,
    );
    assert!(
        weather.iter().any(|name| name == "weather.current"),
        "{weather:?}"
    );
    assert!(
        !weather.iter().any(|name| name == crate::START),
        "{weather:?}"
    );
    let customer = ark_runtime::catalog::matching_actions(
        &registry,
        "Task t-customer delegated by lead (from its task t-lead).\n\n== OBJECTIVE ==\nFind the customer Ada Lovelace: her tier, and the total of all her orders.",
        2,
    );
    assert_eq!(customer.len(), 2);
    assert!(
        customer.iter().all(|name| name.starts_with("crm.")),
        "{customer:?}"
    );
}

#[tokio::test]
async fn client_sends_keep_device_identity_across_restart_and_reject_conflicts() {
    let (_dir, ark, project) = fixture();
    let o = Orchestrator::builder(&ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    o.spawn(AgentSpec::new("reader")).await.unwrap();
    let (phone, duplicate, tablet) = tokio::join!(
        o.submit_once("reader", TaskSpec::new("same brief"), "phone:epoch:1"),
        o.submit_once("reader", TaskSpec::new("same brief"), "phone:epoch:1"),
        o.submit_once("reader", TaskSpec::new("same brief"), "tablet:epoch:1")
    );
    let phone = phone.unwrap();
    assert_eq!(phone, duplicate.unwrap());
    assert_ne!(phone, tablet.unwrap());
    assert!(o
        .submit_once("reader", TaskSpec::new("changed brief"), "phone:epoch:1")
        .await
        .is_err());
    let reloaded = Orchestrator::builder(&ark, &project).build().unwrap();
    assert_eq!(
        phone,
        reloaded
            .submit_once("reader", TaskSpec::new("same brief"), "phone:epoch:1")
            .await
            .unwrap()
    );
    assert_eq!(reloaded.tasks().len(), 2);
}

#[tokio::test]
async fn colleagues_snapshot_execution_and_retries_cannot_change_choices_or_later_edits() {
    let (_dir, ark, project) = fixture();
    let o = Orchestrator::builder(&ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    let task = source(&o, &["agents.create", "files.read"], None).await;
    o.inner
        .state
        .lock()
        .unwrap()
        .tasks
        .get_mut(&task)
        .unwrap()
        .execution_model = Some("provider-a~one:large@high".into());
    o.spawn(
        AgentSpec::new("source")
            .tools(["agents.create", "files.read"])
            .model("provider-b:small"),
    )
    .await
    .unwrap();
    let mut titled = o.agent("source").unwrap().spec;
    titled.title = "Parent conversation".into();
    titled.description = "Parent one-time request".into();
    o.spawn(titled).await.unwrap();
    let inherited = o
        .inner
        .create_colleague(
            "source",
            &task,
            "inherited",
            "review",
            "",
            Default::default(),
        )
        .await
        .unwrap();
    assert!(inherited.spec.title.is_empty());
    assert!(inherited.spec.description.is_empty());
    assert_eq!(
        inherited.spec.model.as_deref(),
        Some("provider-a~one:large@high")
    );
    let selected =
        crate::execution::ExecutionSelection::parse(&serde_json::json!({"reasoning":"low"}))
            .unwrap();
    let changed = o
        .inner
        .create_colleague("source", &task, "chosen", "review", "", selected.clone())
        .await
        .unwrap();
    assert_eq!(
        changed.spec.model.as_deref(),
        Some("provider-a~one:large@low")
    );
    assert_eq!(changed.spec.tools, inherited.spec.tools);
    assert!(!changed.spec.allow_mutate);
    let mut edit = changed.spec.clone();
    edit.role = "Later role".into();
    edit.model = Some("provider-b:small".into());
    o.spawn(edit).await.unwrap();
    let retry = o
        .inner
        .create_colleague("source", &task, "chosen", "review", "", selected)
        .await
        .unwrap();
    assert_eq!(retry.spec.role, "Later role");
    assert_eq!(retry.spec.model.as_deref(), Some("provider-b:small"));
    assert!(o
        .inner
        .create_colleague("source", &task, "chosen", "review", "", Default::default())
        .await
        .is_err());
    let unavailable =
        crate::execution::ExecutionSelection::parse(&serde_json::json!({"model":"missing:model"}))
            .unwrap();
    assert!(o
        .inner
        .create_colleague("source", &task, "refused", "review", "", unavailable)
        .await
        .is_err());
    assert!(o.agent("refused").is_none());
    assert!(o.tasks().iter().all(|t| t.owner == "source"));
}

#[tokio::test]
async fn named_chat_creation_is_atomic_idempotent_and_never_updates_a_duplicate() {
    let (_dir, ark, project) = fixture();
    let o = Orchestrator::builder(&ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    let spec = AgentSpec::new("writer")
        .role("Writes")
        .model("provider-b:small");
    let (a, b) = tokio::join!(
        o.create_chat_agent(spec.clone(), "chat-create:first"),
        o.create_chat_agent(spec.clone(), "chat-create:first")
    );
    assert_eq!(a.unwrap().scope, b.unwrap().scope);
    assert!(o.tasks().is_empty());
    assert!(o
        .create_chat_agent(spec.clone(), "chat-create:changed")
        .await
        .is_err());
    assert!(o
        .create_chat_agent(AgentSpec::new("other-name"), "chat-create:first")
        .await
        .is_err());
    assert!(o.agent("other-name").is_none());
    let reopened = Orchestrator::builder(&ark, &project).build().unwrap();
    assert_eq!(
        reopened
            .create_chat_agent(spec.clone(), "chat-create:first")
            .await
            .unwrap()
            .name,
        "writer"
    );
    let mut edited = spec.clone();
    edited.role = "Edited".into();
    o.spawn(edited).await.unwrap();
    assert_eq!(
        o.create_chat_agent(spec, "chat-create:first")
            .await
            .unwrap()
            .spec
            .role,
        "Edited"
    );
    assert!(o
        .create_chat_agent(
            AgentSpec::new("bad").model("unavailable"),
            "chat-create:bad"
        )
        .await
        .is_err());
    assert!(o.agent("bad").is_none());
}

struct SelectionLookupModels {
    base: Arc<dyn ModelResolver>,
    closed: std::sync::atomic::AtomicUsize,
}

#[async_trait::async_trait]
impl ModelResolver for SelectionLookupModels {
    async fn resolve(&self, lane: &str, spec: Option<&str>) -> Result<AgentModels, OrchError> {
        self.base.resolve(lane, spec).await
    }
    async fn validate_selection(&self, spec: &str) -> Result<(), OrchError> {
        self.base.validate_selection(spec).await
    }
    async fn close(&self, lane: &str) {
        self.closed
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.base.close(lane).await;
    }
}

#[tokio::test]
async fn reasoning_without_model_closes_its_default_lookup_before_refusal_or_creation() {
    let (_dir, ark, project) = fixture();
    let mut builder = crate::Models::builder().default_model("provider-a:large@high");
    for spec in ["provider-a:large@high", "provider-a:large@low"] {
        builder = builder.provider(spec, Arc::new(ark_model::MockProvider::new(spec, vec![])));
    }
    let models = Arc::new(SelectionLookupModels {
        base: Arc::new(builder.build()),
        closed: Default::default(),
    });
    let o = Orchestrator::builder(&ark, &project)
        .models(models.clone())
        .build()
        .unwrap();
    let invalid = crate::execution::ExecutionSelection::parse(
        &serde_json::json!({"reasoning":"unavailable"}),
    )
    .unwrap();
    assert!(o
        .create_agent(AgentSpec::new("invalid"), invalid)
        .await
        .is_err());
    assert!(o.agent("invalid").is_none());
    assert_eq!(models.closed.load(std::sync::atomic::Ordering::SeqCst), 1);
    let valid =
        crate::execution::ExecutionSelection::parse(&serde_json::json!({"reasoning":"low"}))
            .unwrap();
    let created = o
        .create_agent(AgentSpec::new("valid"), valid)
        .await
        .unwrap();
    assert_eq!(created.spec.model.as_deref(), Some("provider-a:large@low"));
    assert_eq!(models.closed.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert!(o.tasks().is_empty());
}

#[tokio::test]
async fn display_metadata_survives_rename_without_becoming_instructions() {
    let (_dir, ark, project) = fixture();
    let o = Orchestrator::builder(&ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    let mut spec = AgentSpec::new("stable-id");
    spec.title = "Review launch".into();
    spec.description = "Only speak French for the first request".into();
    let profile = o.inner.agent_profile(&spec, &[], None);
    assert!(!profile.prompts.instructions.contains(&spec.title));
    assert!(!profile.prompts.instructions.contains(&spec.description));
    let first = o.create_chat_agent(spec.clone(), "first").await.unwrap();
    assert_eq!(o.graph().agents[0].title, spec.title);
    let contacts = crate::agent_contacts(&ark.engine().store, project.id.as_str()).unwrap();
    assert_eq!(contacts[0].description, spec.description);
    spec.title = "Launch review".into();
    let renamed = o.spawn(spec).await.unwrap();
    assert_eq!(renamed.name, first.name);
    assert_eq!(renamed.scope, first.scope);
    assert_eq!(renamed.creation_request, first.creation_request);
    let reopened = Orchestrator::builder(&ark, &project).build().unwrap();
    assert_eq!(
        reopened.agent("stable-id").unwrap().spec.title,
        "Launch review"
    );
    let legacy: AgentSpec = serde_json::from_value(serde_json::json!({"name":"old"})).unwrap();
    assert!(legacy.title.is_empty());
    assert!(legacy.description.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn durable_work_edits_and_claims_do_not_overwrite_each_other() {
    let (_dir, ark, project) = fixture();
    let first = Orchestrator::builder(&ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    first.spawn(AgentSpec::new("reader")).await.unwrap();
    let second = Orchestrator::builder(&ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    for index in 0..16 {
        let item = first
            .add_work(
                crate::board::PERSON,
                &format!("work {index}"),
                crate::WorkChange::default(),
            )
            .unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let edit = {
            let o = first.clone();
            let barrier = barrier.clone();
            let id = item.id.clone();
            tokio::task::spawn_blocking(move || {
                barrier.wait();
                o.change_work(
                    crate::board::PERSON,
                    &id,
                    crate::WorkChange {
                        note: Some("edited".into()),
                        ..Default::default()
                    },
                )
            })
        };
        let claim = {
            let o = second.clone();
            let barrier = barrier.clone();
            let id = item.id.clone();
            tokio::task::spawn_blocking(move || {
                barrier.wait();
                o.inner.create_task(
                    "reader",
                    TaskSpec::new("claim").item(id),
                    Origin::External,
                    None,
                )
            })
        };
        edit.await.unwrap().unwrap();
        let task = claim.await.unwrap().unwrap();
        let saved: crate::WorkItem = serde_json::from_value(
            ark.engine()
                .store
                .get_record(crate::board::WORK, &item.id)
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(saved.note, "edited");
        assert_eq!(saved.tasks, vec![task]);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleting_work_or_its_board_cannot_leave_recreated_or_orphaned_items() {
    let (_dir, ark, project) = fixture();
    let first = Orchestrator::builder(&ark, &project).build().unwrap();
    let second = Orchestrator::builder(&ark, &project).build().unwrap();
    for index in 0..16 {
        let board = first
            .add_board(crate::board::PERSON, &format!("board {index}"), None)
            .unwrap();
        let item = first
            .add_work(
                crate::board::PERSON,
                "old",
                crate::WorkChange {
                    board: Some(board.id.clone()),
                    ..Default::default()
                },
            )
            .unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let add = {
            let o = first.clone();
            let barrier = barrier.clone();
            let board = board.id.clone();
            tokio::task::spawn_blocking(move || {
                barrier.wait();
                o.add_work(
                    crate::board::PERSON,
                    "new",
                    crate::WorkChange {
                        board: Some(board),
                        ..Default::default()
                    },
                )
            })
        };
        let remove = {
            let o = second.clone();
            let barrier = barrier.clone();
            let board = board.id.clone();
            tokio::task::spawn_blocking(move || {
                barrier.wait();
                o.remove_board(&board)
            })
        };
        let _add_result = add.await.unwrap();
        remove.await.unwrap().unwrap();
        assert!(first
            .change_work(
                crate::board::PERSON,
                &item.id,
                crate::WorkChange {
                    note: Some("stale edit".into()),
                    ..Default::default()
                }
            )
            .is_err());
        assert!(ark
            .engine()
            .store
            .get_record(crate::board::BOARD, &board.id)
            .unwrap()
            .is_none());
        let items = ark
            .engine()
            .store
            .list_records(crate::board::WORK, Some(project.id.as_str()))
            .unwrap();
        assert!(items.iter().all(|item| item["board"] != board.id));
    }
}

#[tokio::test]
async fn a_refused_retry_does_not_queue_or_change_the_failed_task() {
    let (_dir, ark, project) = fixture();
    let o = Orchestrator::builder(&ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    o.spawn(AgentSpec::new("first")).await.unwrap();
    o.spawn(AgentSpec::new("second")).await.unwrap();
    let item = o
        .add_work(crate::board::PERSON, "work", crate::WorkChange::default())
        .unwrap();
    let failed = o
        .inner
        .create_task(
            "first",
            TaskSpec::new("old").item(&item.id),
            Origin::External,
            None,
        )
        .unwrap();
    o.inner
        .finish(&failed, Final::failed("original failure"))
        .await;
    let next = o
        .inner
        .create_task(
            "second",
            TaskSpec::new("new").item(&item.id),
            Origin::External,
            None,
        )
        .unwrap();
    assert!(o.retry(&failed).is_err());
    let memory = o.task(&failed).unwrap();
    assert_eq!(memory.state, TaskState::Failed);
    assert_eq!(memory.error.as_deref(), Some("original failure"));
    let durable: TaskRecord = serde_json::from_value(
        ark.engine()
            .store
            .get_record(TASK, &failed)
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(durable.state, TaskState::Failed);
    assert_eq!(durable.error, memory.error);
    assert_eq!(
        o.work_item(&item.id).unwrap().owner.as_deref(),
        Some("second")
    );
    assert_eq!(o.task(&next).unwrap().state, TaskState::Queued);
}

#[tokio::test]
async fn work_admission_is_atomic_scoped_and_keeps_complete_request_identity() {
    let (dir, ark, project) = fixture();
    let first = Orchestrator::builder(&ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    first.spawn(AgentSpec::new("reader")).await.unwrap();
    let second = Orchestrator::builder(&ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    let target = crate::WorkTarget::Agent {
        name: "reader".into(),
    };
    let spec = TaskSpec::new("same objective").context("same context");
    let (a, b) = tokio::join!(
        first.admit_work(target.clone(), spec.clone(), "desktop", "one"),
        second.admit_work(target.clone(), spec.clone(), "desktop", "one"),
    );
    let receipt = a.unwrap();
    assert_eq!(receipt, b.unwrap());
    assert_eq!(
        ark.engine()
            .store
            .list_records(TASK, Some(project.id.as_str()))
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        ark.engine()
            .store
            .list_records(crate::board::WORK, Some(project.id.as_str()))
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        ark.engine()
            .store
            .list_sessions(Some(&project.id))
            .unwrap()
            .len(),
        1
    );
    let mut changed = spec.clone();
    changed.max_attempts = Some(2);
    assert!(second
        .admit_work(target.clone(), changed, "desktop", "one")
        .await
        .is_err());
    assert!(second
        .admit_work(
            crate::WorkTarget::Agent {
                name: "other".into()
            },
            spec.clone(),
            "desktop",
            "one"
        )
        .await
        .is_err());
    let another = first
        .admit_work(target.clone(), spec.clone(), "desktop", "two")
        .await
        .unwrap();
    assert_ne!(receipt.conversation, another.conversation);
    assert_ne!(receipt.work, another.work);
    let other_origin = first
        .admit_work(target.clone(), spec.clone(), "phone", "one")
        .await
        .unwrap();
    assert_ne!(receipt.id, other_origin.id);
    let mut followup = TaskSpec::new("follow up").item(&receipt.work);
    let followup_receipt = first
        .admit_work(target.clone(), followup.clone(), "desktop", "three")
        .await
        .unwrap();
    assert_eq!(followup_receipt.conversation, receipt.conversation);
    assert_eq!(followup_receipt.work, receipt.work);
    assert_ne!(followup_receipt.task, receipt.task);
    first.archive_work("person-a", &receipt.work, true).unwrap();
    assert!(first
        .work_item(&receipt.work)
        .unwrap()
        .archived_by
        .contains(&"person-a".into()));
    first
        .archive_work("person-b", &receipt.work, false)
        .unwrap();
    assert!(first
        .work_item(&receipt.work)
        .unwrap()
        .archived_by
        .contains(&"person-a".into()));
    first
        .archive_work("person-a", &receipt.work, false)
        .unwrap();
    assert!(first
        .work_item(&receipt.work)
        .unwrap()
        .archived_by
        .is_empty());
    let before_tasks = ark
        .engine()
        .store
        .list_records(TASK, Some(project.id.as_str()))
        .unwrap()
        .len();
    let before_work = ark
        .engine()
        .store
        .list_records(crate::board::WORK, Some(project.id.as_str()))
        .unwrap()
        .len();
    let before_sessions = ark
        .engine()
        .store
        .list_sessions(Some(&project.id))
        .unwrap()
        .len();
    let db = rusqlite::Connection::open(dir.path().join("meta.db")).unwrap();
    db.execute_batch("CREATE TRIGGER reject_submission BEFORE INSERT ON records WHEN NEW.kind='orch_work_submission' BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    followup.item = None;
    assert!(first
        .admit_work(target, followup, "desktop", "fail")
        .await
        .is_err());
    assert_eq!(
        ark.engine()
            .store
            .list_records(TASK, Some(project.id.as_str()))
            .unwrap()
            .len(),
        before_tasks
    );
    assert_eq!(
        ark.engine()
            .store
            .list_records(crate::board::WORK, Some(project.id.as_str()))
            .unwrap()
            .len(),
        before_work
    );
    assert_eq!(
        ark.engine()
            .store
            .list_sessions(Some(&project.id))
            .unwrap()
            .len(),
        before_sessions
    );
    assert!(first.work_receipt("desktop", "fail").unwrap().is_none());
}

#[tokio::test]
async fn general_admission_creates_no_agent_and_recovers_receipt_without_a_model() {
    let (_dir, ark, project) = fixture();
    let o = Orchestrator::builder(&ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    let target = crate::WorkTarget::General { model: None };
    let receipt = o
        .admit_work(
            target.clone(),
            TaskSpec::new("general request"),
            "desktop",
            "one",
        )
        .await
        .unwrap();
    assert!(o.agents().is_empty());
    assert!(receipt.task.is_none());
    assert!(o.tasks().is_empty());
    drop(o);
    let reopened = Orchestrator::builder(&ark, &project).build().unwrap();
    assert_eq!(
        reopened
            .admit_work(target, TaskSpec::new("general request"), "desktop", "one")
            .await
            .unwrap(),
        receipt
    );
    assert_eq!(
        reopened
            .work_item(&receipt.work)
            .unwrap()
            .conversation
            .as_deref(),
        Some(receipt.conversation.as_str())
    );
    assert!(reopened.agents().is_empty());
}

#[tokio::test]
async fn synthetic_provider_keeps_work_threads_apart_from_the_agents_primary_chat() {
    let (dir, _unused, _project) = fixture();
    let reply = |content: &str| ark_model::Completion {
        content: content.into(),
        tool_calls: vec![],
        usage: Default::default(),
    };
    let provider = Arc::new(ark_model::MockProvider::new(
        "test",
        vec![
            reply("LEGACY_PRIVATE"),
            reply("ALPHA_PRIVATE"),
            reply("BETA_PRIVATE"),
            reply("FOLLOW_ALPHA"),
        ],
    ));
    let ark = Ark::builder()
        .store_root(dir.path().join("isolated-provider"))
        .provider(provider.clone())
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.create_project("project", "test").unwrap();
    let models = Arc::new(
        crate::Models::builder()
            .default_model("test")
            .provider("test", provider.clone())
            .build(),
    );
    let o = Orchestrator::builder(&ark, &project)
        .models(models)
        .build()
        .unwrap();
    o.spawn(AgentSpec::new("reader")).await.unwrap();
    o.run("reader", TaskSpec::new("legacy private objective"))
        .await
        .unwrap();
    let primary = o.agent("reader").unwrap().session.unwrap();
    let target = crate::WorkTarget::Agent {
        name: "reader".into(),
    };
    let alpha = o
        .admit_work(
            target.clone(),
            TaskSpec::new("alpha objective"),
            "desktop",
            "alpha",
        )
        .await
        .unwrap();
    o.inner.start(alpha.task.as_deref().unwrap()).unwrap();
    assert_eq!(
        o.wait(alpha.task.as_deref().unwrap())
            .await
            .unwrap()
            .result
            .as_deref(),
        Some("ALPHA_PRIVATE")
    );
    let beta = o
        .admit_work(
            target.clone(),
            TaskSpec::new("beta objective"),
            "desktop",
            "beta",
        )
        .await
        .unwrap();
    o.inner.start(beta.task.as_deref().unwrap()).unwrap();
    assert_eq!(
        o.wait(beta.task.as_deref().unwrap())
            .await
            .unwrap()
            .result
            .as_deref(),
        Some("BETA_PRIVATE")
    );
    let follow = o
        .admit_work(
            target,
            TaskSpec::new("follow alpha").item(&alpha.work),
            "desktop",
            "follow",
        )
        .await
        .unwrap();
    o.inner.start(follow.task.as_deref().unwrap()).unwrap();
    o.wait(follow.task.as_deref().unwrap()).await.unwrap();
    assert_eq!(
        o.agent("reader").unwrap().session.as_deref(),
        Some(primary.as_str())
    );
    assert_eq!(
        Session::open(ark.engine(), &primary)
            .unwrap()
            .unwrap()
            .turn_count(),
        1
    );
    assert_eq!(
        Session::open(ark.engine(), &alpha.conversation)
            .unwrap()
            .unwrap()
            .turn_count(),
        2
    );
    assert_eq!(
        Session::open(ark.engine(), &beta.conversation)
            .unwrap()
            .unwrap()
            .turn_count(),
        1
    );
    let calls = provider.recorded();
    assert_eq!(calls.len(), 4);
    let beta_input = serde_json::to_string(&calls[2].messages).unwrap();
    assert!(!beta_input.contains("LEGACY_PRIVATE"));
    assert!(!beta_input.contains("ALPHA_PRIVATE"));
    let follow_input = serde_json::to_string(&calls[3].messages).unwrap();
    assert!(follow_input.contains("ALPHA_PRIVATE"));
    assert!(!follow_input.contains("BETA_PRIVATE"));
    assert!(!follow_input.contains("LEGACY_PRIVATE"));
}

struct BlockingWorkProvider {
    calls: std::sync::atomic::AtomicUsize,
    entered: tokio::sync::Notify,
    released: tokio::sync::Semaphore,
}

#[async_trait::async_trait]
impl ark_model::Provider for BlockingWorkProvider {
    fn describe(&self) -> String {
        "synthetic blocked work provider".into()
    }
    fn model_id(&self) -> String {
        "blocked-work".into()
    }
    async fn complete(
        &self,
        _system: &str,
        _messages: &[ark_model::Message],
        _tools: &[ark_model::ToolDef],
    ) -> ark_model::Result<ark_model::Completion> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.entered.notify_one();
        self.released.acquire().await.unwrap().forget();
        Ok(ark_model::Completion {
            content: "synthetic answer".into(),
            tool_calls: vec![],
            usage: Default::default(),
        })
    }
}

#[tokio::test]
async fn repeated_start_preserves_the_live_record_before_its_attempt_is_persisted() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(BlockingWorkProvider {
        calls: Default::default(),
        entered: Default::default(),
        released: tokio::sync::Semaphore::new(0),
    });
    let ark = Ark::builder()
        .store_root(dir.path())
        .provider(provider.clone())
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.create_project("project", "test").unwrap();
    let models = Arc::new(
        crate::Models::builder()
            .default_model("blocked")
            .provider("blocked", provider.clone())
            .build(),
    );
    let o = Orchestrator::builder(&ark, &project)
        .models(models)
        .build()
        .unwrap();
    o.spawn(AgentSpec::new("reader")).await.unwrap();
    let receipt = o
        .admit_work(
            crate::WorkTarget::Agent {
                name: "reader".into(),
            },
            TaskSpec::new("one request"),
            "desktop",
            "one",
        )
        .await
        .unwrap();
    let id = receipt.task.as_deref().unwrap();
    let (reserved, lease) = o.inner.reserve_execution(id).unwrap();
    assert!(lease.is_some());
    {
        let mut state = o.inner.state.lock().unwrap();
        let task = state.tasks.get_mut(id).unwrap();
        task.record = reserved.clone();
        task.record.state = TaskState::Running;
        task.record.attempts = 1;
    }
    o.execute_work(&receipt.id).unwrap();
    let observed = o.task(id).unwrap();
    assert_eq!(observed.attempts, 1);
    assert_eq!(observed.state, TaskState::Running);
    assert_eq!(observed.execution_owner, reserved.execution_owner);
    assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    drop(lease);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_work_has_one_executor_and_other_instances_observe_without_restarting_it() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(BlockingWorkProvider {
        calls: Default::default(),
        entered: Default::default(),
        released: tokio::sync::Semaphore::new(0),
    });
    let ark = Ark::builder()
        .store_root(dir.path())
        .provider(provider.clone())
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.create_project("project", "test").unwrap();
    let models = Arc::new(
        crate::Models::builder()
            .default_model("blocked")
            .provider("blocked", provider.clone())
            .build(),
    );
    let first = Orchestrator::builder(&ark, &project)
        .models(models.clone())
        .build()
        .unwrap();
    first.spawn(AgentSpec::new("reader")).await.unwrap();
    let target = crate::WorkTarget::Agent {
        name: "reader".into(),
    };
    let receipt = first
        .admit_work(
            target.clone(),
            TaskSpec::new("one request"),
            "desktop",
            "one",
        )
        .await
        .unwrap();
    let second = Orchestrator::builder(&ark, &project)
        .models(models.clone())
        .build()
        .unwrap();
    first.execute_work(&receipt.id).unwrap();
    second.execute_work(&receipt.id).unwrap();
    tokio::time::timeout(Duration::from_secs(3), provider.entered.notified())
        .await
        .unwrap();
    let observer = Orchestrator::builder(&ark, &project)
        .models(models)
        .build()
        .unwrap();
    assert!(observer.cancel(receipt.task.as_deref().unwrap()).is_err());
    assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    provider.released.add_permits(1);
    let (a, b) = tokio::join!(
        first.wait(receipt.task.as_deref().unwrap()),
        observer.wait(receipt.task.as_deref().unwrap())
    );
    assert_eq!(a.unwrap().result.as_deref(), Some("synthetic answer"));
    assert_eq!(b.unwrap().attempts, 1);
    assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    let left = first
        .admit_work(target.clone(), TaskSpec::new("left"), "desktop", "left")
        .await
        .unwrap();
    let right = second
        .admit_work(target, TaskSpec::new("right"), "desktop", "right")
        .await
        .unwrap();
    first.execute_work(&left.id).unwrap();
    tokio::time::timeout(Duration::from_secs(3), provider.entered.notified())
        .await
        .unwrap();
    second.execute_work(&right.id).unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if provider.calls.load(std::sync::atomic::Ordering::SeqCst) > 2
                || second
                    .task(right.task.as_deref().unwrap())
                    .unwrap()
                    .waiting_on
                    .as_deref()
                    .is_some_and(|reason| reason.contains("reader is busy"))
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    provider.released.add_permits(2);
    let (left_done, right_done) = tokio::join!(
        first.wait(left.task.as_deref().unwrap()),
        second.wait(right.task.as_deref().unwrap())
    );
    assert_eq!(left_done.unwrap().state, TaskState::Done);
    assert_eq!(right_done.unwrap().state, TaskState::Done);
    assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 3);
}

#[tokio::test]
async fn a_saved_answer_repairs_its_conversation_without_calling_the_provider_again() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ark_model::MockProvider::new(
        "test",
        vec![ark_model::Completion {
            content: "retained answer".into(),
            tool_calls: vec![],
            usage: Default::default(),
        }],
    ));
    let ark = Ark::builder()
        .store_root(dir.path())
        .provider(provider.clone())
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.create_project("project", "test").unwrap();
    let models = Arc::new(
        crate::Models::builder()
            .default_model("test")
            .provider("test", provider.clone())
            .build(),
    );
    let o = Orchestrator::builder(&ark, &project)
        .models(models)
        .build()
        .unwrap();
    o.spawn(AgentSpec::new("reader")).await.unwrap();
    let receipt = o
        .admit_work(
            crate::WorkTarget::Agent {
                name: "reader".into(),
            },
            TaskSpec::new("answer once"),
            "desktop",
            "one",
        )
        .await
        .unwrap();
    let db = rusqlite::Connection::open(dir.path().join("meta.db")).unwrap();
    db.execute_batch("CREATE TRIGGER reject_answer_header BEFORE UPDATE ON sessions BEGIN SELECT RAISE(ABORT,'injected answer header failure'); END;").unwrap();
    o.execute_work(&receipt.id).unwrap();
    let failed = tokio::time::timeout(
        Duration::from_secs(3),
        o.wait(receipt.task.as_deref().unwrap()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(failed.state, TaskState::Failed);
    assert_eq!(failed.result.as_deref(), Some("retained answer"));
    assert!(failed
        .error
        .as_deref()
        .unwrap()
        .contains("conversation could not be saved"));
    assert!(o.retry(&failed.id).is_err());
    assert_eq!(provider.call_count(), 1);
    assert_eq!(
        ark.engine()
            .store
            .get_session(&receipt.conversation)
            .unwrap()
            .unwrap()["turn_count"],
        0
    );
    db.execute_batch("DROP TRIGGER reject_answer_header;")
        .unwrap();
    o.execute_work(&receipt.id).unwrap();
    let done = tokio::time::timeout(Duration::from_secs(3), o.wait(&failed.id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(done.state, TaskState::Done);
    assert_eq!(done.result.as_deref(), Some("retained answer"));
    assert_eq!(provider.call_count(), 1);
    let turns = ark
        .engine()
        .store
        .list_session_turns(&receipt.conversation, None)
        .unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0]["answer"], "retained answer");
    assert_eq!(
        ark.engine()
            .store
            .get_session(&receipt.conversation)
            .unwrap()
            .unwrap()["turn_count"],
        1
    );
    o.execute_work(&receipt.id).unwrap();
    assert_eq!(provider.call_count(), 1);
}

#[test]
fn general_admission_crash_child() {
    let Ok(root) = std::env::var("ARK_GENERAL_CRASH_CHILD") else {
        return;
    };
    let mode = std::env::var("ARK_GENERAL_CRASH_STAGE").unwrap();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let provider = Arc::new(ark_model::MockProvider::new("test", vec![]));
        let ark = Ark::builder()
            .store_root(root)
            .provider(provider)
            .planner(false)
            .critic(false)
            .build()
            .unwrap();
        let project = ark.create_project("project", "test").unwrap();
        let o = Orchestrator::builder(&ark, &project)
            .models(test_models())
            .build()
            .unwrap();
        let receipt = o
            .admit_work(
                crate::WorkTarget::General { model: None },
                TaskSpec::new("one real admitted message"),
                "desktop",
                "crash-key",
            )
            .await
            .unwrap();
        let crate::GeneralWorkStart::Ready { input, .. } =
            o.begin_general_work(&receipt.id).unwrap()
        else {
            panic!("new work was not ready");
        };
        if mode == "begun" {
            let mut session = ark.open_session(&receipt.conversation).unwrap().unwrap();
            session.begin_turn(ark.engine(), &input).unwrap();
        }
        use std::io::Write;
        println!("READY {} {}", project.id, receipt.id);
        std::io::stdout().flush().unwrap();
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).unwrap();
    });
}

#[tokio::test]
async fn general_recovery_restarts_only_before_a_turn_was_durably_begun() {
    use std::io::BufRead;
    use std::process::{Command, Stdio};
    for stage in ["claimed", "begun"] {
        let dir = tempfile::tempdir().unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "orchestrator::conversation_tests::general_admission_crash_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("ARK_GENERAL_CRASH_CHILD", dir.path())
            .env("ARK_GENERAL_CRASH_STAGE", stage)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut output = std::io::BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        loop {
            line.clear();
            assert!(
                output.read_line(&mut line).unwrap() > 0,
                "child ended before admission"
            );
            if let Some(start) = line.find("READY ") {
                line = line[start..].to_string();
                break;
            }
        }
        let ids: Vec<_> = line.split_whitespace().map(str::to_string).collect();
        child.kill().unwrap();
        child.wait().unwrap();
        let provider = Arc::new(ark_model::MockProvider::new("test", vec![]));
        let ark = Ark::builder()
            .store_root(dir.path())
            .provider(provider.clone())
            .planner(false)
            .critic(false)
            .build()
            .unwrap();
        let project = ark
            .engine()
            .store
            .get_project(&ProjectId(ids[1].clone()))
            .unwrap()
            .unwrap();
        let o = Orchestrator::builder(&ark, &project)
            .models(test_models())
            .build()
            .unwrap();
        let duplicate = o
            .admit_work(
                crate::WorkTarget::General { model: None },
                TaskSpec::new("one real admitted message"),
                "desktop",
                "crash-key",
            )
            .await
            .unwrap();
        assert_eq!(duplicate.id, ids[2]);
        if stage == "claimed" {
            assert!(matches!(
                o.begin_general_work(&duplicate.id).unwrap(),
                crate::GeneralWorkStart::Ready { .. }
            ));
            assert!(matches!(
                o.begin_general_work(&duplicate.id).unwrap(),
                crate::GeneralWorkStart::Observed {
                    state: crate::GeneralWorkState::Starting,
                    ..
                }
            ));
            o.release_unstarted_general_work(&duplicate.id).unwrap();
            assert_eq!(
                o.general_work_state(&duplicate.id).unwrap(),
                crate::GeneralWorkState::Pending
            );
        } else {
            assert!(matches!(
                o.begin_general_work(&duplicate.id).unwrap(),
                crate::GeneralWorkStart::Observed {
                    state: crate::GeneralWorkState::Interrupted,
                    ..
                }
            ));
            assert!(o.release_unstarted_general_work(&duplicate.id).is_err());
        }
        assert!(o.agents().is_empty());
        assert_eq!(provider.call_count(), 0);
        assert_eq!(
            ark.engine()
                .store
                .list_sessions(Some(&project.id))
                .unwrap()
                .len(),
            1
        );
    }
}

#[tokio::test]
async fn general_work_records_a_real_sdk_turn_and_keeps_its_result_after_conversation_removal() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ark_model::MockProvider::new(
        "test",
        vec![ark_model::Completion {
            content: "synthetic General answer".into(),
            tool_calls: vec![],
            usage: Default::default(),
        }],
    ));
    let ark = Ark::builder()
        .store_root(dir.path())
        .provider(provider.clone())
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.create_project("project", "test").unwrap();
    let models = Arc::new(
        crate::Models::builder()
            .default_model("test")
            .provider("test", provider.clone())
            .build(),
    );
    let o = Orchestrator::builder(&ark, &project)
        .models(models)
        .build()
        .unwrap();
    let receipt = o
        .admit_work(
            crate::WorkTarget::General { model: None },
            TaskSpec::new("one General request"),
            "desktop",
            "one",
        )
        .await
        .unwrap();
    let before = o.work_item(&receipt.work).unwrap();
    let refused = o
        .change_work(
            crate::board::PERSON,
            &receipt.work,
            crate::board::WorkChange {
                state: Some(WorkState::Done),
                ..Default::default()
            },
        )
        .unwrap_err();
    assert!(refused.to_string().contains("wait for this execution"));
    assert_eq!(o.work_item(&receipt.work).unwrap().state, before.state);
    let crate::GeneralWorkStart::Ready { input, .. } = o.begin_general_work(&receipt.id).unwrap()
    else {
        panic!("expected one initial turn");
    };
    assert!(matches!(
        o.begin_general_work(&receipt.id).unwrap(),
        crate::GeneralWorkStart::Observed {
            state: crate::GeneralWorkState::Starting,
            ..
        }
    ));
    assert!(o
        .admit_work(
            crate::WorkTarget::General { model: None },
            TaskSpec::new("too soon").item(&receipt.work),
            "desktop",
            "two"
        )
        .await
        .is_err());
    let mut session = ark.open_session(&receipt.conversation).unwrap().unwrap();
    ark.ask_session(
        &mut session,
        &input,
        ark_sdk::TurnOptions::default(),
        |_| {},
    )
    .await
    .unwrap();
    assert_eq!(
        o.general_work_state(&receipt.id).unwrap(),
        crate::GeneralWorkState::Completed
    );
    let result = o.settle_general_work(&receipt.id).unwrap().unwrap();
    assert_eq!(result.answer.as_deref(), Some("synthetic General answer"));
    assert_eq!(o.work_item(&receipt.work).unwrap().state, WorkState::Done);
    assert!(matches!(
        o.begin_general_work(&receipt.id).unwrap(),
        crate::GeneralWorkStart::Observed {
            state: crate::GeneralWorkState::Completed,
            ..
        }
    ));
    assert_eq!(provider.call_count(), 1);
    let db = rusqlite::Connection::open(dir.path().join("meta.db")).unwrap();
    db.execute(
        "DELETE FROM session_turns WHERE session_id=?1",
        [&receipt.conversation],
    )
    .unwrap();
    db.execute("DELETE FROM sessions WHERE id=?1", [&receipt.conversation])
        .unwrap();
    let page = o.work_page("person-a", false, None, 1).unwrap();
    assert_eq!(page.entries.len(), 1);
    assert!(page.entries[0].conversation_missing);
    assert_eq!(
        page.entries[0].general[0]
            .2
            .as_ref()
            .unwrap()
            .answer
            .as_deref(),
        Some("synthetic General answer")
    );
    assert_eq!(
        o.settle_general_work(&receipt.id).unwrap().unwrap().answer,
        result.answer
    );
    o.archive_work("person-a", &receipt.work, true).unwrap();
    assert!(o
        .work_page("person-a", false, None, 10)
        .unwrap()
        .entries
        .is_empty());
    assert_eq!(
        o.work_page("person-b", false, None, 10)
            .unwrap()
            .entries
            .len(),
        1
    );
    assert_eq!(
        o.work_page("person-a", true, None, 10)
            .unwrap()
            .entries
            .len(),
        1
    );
    assert!(o.agents().is_empty());
}

#[tokio::test]
async fn cancelled_general_receipt_survives_reopen_and_cannot_claim_a_later_turn() {
    let (_dir, ark, project) = fixture();
    let o = Orchestrator::builder(&ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    let receipt = o
        .admit_work(
            crate::WorkTarget::General { model: None },
            TaskSpec::new("cancel me"),
            "desktop",
            "cancel",
        )
        .await
        .unwrap();
    assert!(matches!(
        o.begin_general_work(&receipt.id).unwrap(),
        crate::GeneralWorkStart::Ready { .. }
    ));
    assert!(o.cancel_unstarted_general_work(&receipt.id).unwrap());
    assert!(!o.cancel_unstarted_general_work(&receipt.id).unwrap());
    assert!(o
        .admit_work(
            crate::WorkTarget::General { model: None },
            TaskSpec::new("too soon").item(&receipt.work),
            "desktop",
            "early"
        )
        .await
        .is_err());
    o.end_general_work(&receipt.id).unwrap();
    drop(o);
    let o = Orchestrator::builder(&ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    assert_eq!(
        o.general_work_state(&receipt.id).unwrap(),
        crate::GeneralWorkState::Cancelled
    );
    assert!(matches!(
        o.begin_general_work(&receipt.id).unwrap(),
        crate::GeneralWorkStart::Observed {
            state: crate::GeneralWorkState::Cancelled,
            ..
        }
    ));
    let next = o
        .admit_work(
            crate::WorkTarget::General { model: None },
            TaskSpec::new("new message").item(&receipt.work),
            "desktop",
            "next",
        )
        .await
        .unwrap();
    assert_eq!(next.turn, receipt.turn);
    assert_eq!(
        o.general_work_state(&receipt.id).unwrap(),
        crate::GeneralWorkState::Cancelled
    );
    assert_eq!(
        o.general_work_state(&next.id).unwrap(),
        crate::GeneralWorkState::Pending
    );
}

fn sequence_draft() -> crate::automation::Draft {
    use crate::automation::*;
    Draft {
        name: "Review and retain".into(),
        trigger: Trigger::Manual,
        inputs: vec![],
        authority: Authority::default(),
        steps: vec![
            Step {
                id: "review".into(),
                name: "Review".into(),
                action: StepAction::Agent {
                    target: WorkTarget::General {
                        model: Some("test".into()),
                    },
                    prompt: "Review the supplied text".into(),
                    inputs: vec![],
                },
            },
            Step {
                id: "result".into(),
                name: "Result".into(),
                action: StepAction::Result {
                    inputs: vec![Binding::Step {
                        step: "review".into(),
                        port: Port::Text,
                    }],
                },
            },
        ],
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn automation_revisions_are_idempotent_conflict_checked_and_layout_does_not_change_authority()
{
    use crate::automation::*;
    let (_dir, ark, project) = fixture();
    let o = Orchestrator::builder(&ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    let other = Orchestrator::builder(&ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    let draft = sequence_draft();
    let saved = o
        .save_automation("create", None, None, draft.clone())
        .unwrap();
    assert_eq!(
        serde_json::to_value(
            o.save_automation("create", None, None, draft.clone())
                .unwrap()
        )
        .unwrap(),
        serde_json::to_value(&saved).unwrap()
    );
    let mut changed = draft.clone();
    changed.name = "Another title".into();
    assert!(o
        .save_automation("create", None, None, changed.clone())
        .is_err());
    let a = o.clone();
    let b = other.clone();
    let aid = saved.automation.clone();
    let bid = aid.clone();
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let barrier2 = barrier.clone();
    let da = changed.clone();
    let db = changed;
    let first = std::thread::spawn(move || {
        barrier.wait();
        a.save_automation("edit-a", Some(&aid), Some(1), da)
    });
    let second = std::thread::spawn(move || {
        barrier2.wait();
        b.save_automation("edit-b", Some(&bid), Some(1), db)
    });
    let first = first.join().unwrap();
    let second = second.join().unwrap();
    assert_ne!(first.is_ok(), second.is_ok());
    let entry = o.automation(&saved.automation).unwrap();
    assert_eq!(entry.definition.revision, 2);
    let digest = entry.revision.digest.clone();
    let layout = Layout {
        version: 0,
        positions: std::collections::BTreeMap::from([(
            "review".into(),
            Position { x: 120.0, y: 40.0 },
        )]),
    };
    assert_eq!(
        o.save_automation_layout(&saved.automation, 0, layout.clone())
            .unwrap()
            .version,
        1
    );
    assert!(o
        .save_automation_layout(&saved.automation, 0, layout)
        .is_err());
    assert_eq!(
        o.automation(&saved.automation).unwrap().revision.digest,
        digest
    );
    assert_eq!(
        o.automation(&saved.automation).unwrap().definition.revision,
        2
    );
    let prepared = o
        .prepare_automation_activation(&saved.automation, 2)
        .await
        .unwrap();
    assert_eq!(prepared.phase, ActivationPhase::Prepared);
    assert_eq!(prepared.selections["review"].model, "test");
    assert!(o.agents().is_empty());
    assert!(!o.automation(&saved.automation).unwrap().definition.enabled);
}

#[tokio::test]
async fn webhook_input_preview_maps_named_ports_without_execution() {
    use crate::automation::*;
    let (_dir, ark, project) = fixture();
    let o = Orchestrator::builder(&ark, &project).build().unwrap();
    let mut draft = sequence_draft();
    draft.trigger = Trigger::Webhook;
    draft.inputs = vec![
        Input {
            name: "message".into(),
            port: Port::Text,
            required: true,
        },
        Input {
            name: "files".into(),
            port: Port::Files,
            required: false,
        },
    ];
    draft.steps[0].action = StepAction::Result {
        inputs: vec![Binding::Input {
            name: "message".into(),
            port: Port::Text,
        }],
    };
    let saved = o
        .save_automation("preview-only", None, None, draft)
        .unwrap();
    let inputs = std::collections::BTreeMap::from([(
        "message".into(),
        InputValue::Text {
            value: "Synthetic event acceptance".into(),
        },
    )]);
    let preview = o
        .preview_webhook_inputs(&saved.automation, 1, inputs.clone())
        .unwrap();
    assert_eq!(preview.revision, 1);
    assert_eq!(preview.inputs, inputs);
    assert_eq!(preview.bindings.len(), 1);
    assert_eq!(preview.bindings[0].input, "message");
    assert_eq!(preview.bindings[0].step, "review");
    assert!(o
        .preview_webhook_inputs(&saved.automation, 2, inputs.clone())
        .is_err());
    assert!(o
        .preview_webhook_inputs(&saved.automation, 1, Default::default())
        .is_err());
    let mut invalid = inputs.clone();
    invalid.insert("message".into(), InputValue::Files { ids: vec![] });
    assert!(o
        .preview_webhook_inputs(&saved.automation, 1, invalid)
        .is_err());
    let mut invalid = inputs;
    invalid.insert(
        "files".into(),
        InputValue::Files {
            ids: vec!["foreign-file".into()],
        },
    );
    assert!(o
        .preview_webhook_inputs(&saved.automation, 1, invalid)
        .is_err());
    assert!(o
        .automation_history(&saved.automation, None, 10)
        .unwrap()
        .runs
        .is_empty());
    assert!(o
        .work_page("person", false, None, 50)
        .unwrap()
        .entries
        .is_empty());
    assert!(o.agents().is_empty());
}

#[tokio::test]
async fn scheduled_required_inputs_block_activation_but_allow_an_explicit_manual_start() {
    use crate::automation::*;
    let (_dir, ark, project) = fixture();
    let o = Orchestrator::builder(&ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    let mut draft = sequence_draft();
    draft.trigger = Trigger::Schedule {
        when: crate::When::daily_at("09:00"),
    };
    draft.inputs = vec![Input {
        name: "message".into(),
        port: Port::Text,
        required: true,
    }];
    assert!(o.validate_automation_draft(&draft).unwrap().is_empty());
    assert_eq!(draft.activation_issues()[0].field, "Execution inputs");
    let saved = o
        .save_automation("scheduled-inputs", None, None, draft)
        .unwrap();
    let error = o
        .prepare_automation_activation(&saved.automation, 1)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("Schedules cannot supply required inputs"));
    assert!(o
        .admit_manual_automation_run(ManualStart {
            previous: None,
            automation: saved.automation,
            expected: 1,
            request: "explicit-inputs".into(),
            inputs: std::collections::BTreeMap::from([(
                "message".into(),
                InputValue::Text {
                    value: "Manual value".into()
                }
            )])
        })
        .await
        .is_ok());
}

#[tokio::test]
async fn automation_keeps_incomplete_drafts_but_refuses_execution_and_ninth_steps() {
    use crate::automation::*;
    let (_dir, ark, project) = fixture();
    let o = Orchestrator::builder(&ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    let mut draft = sequence_draft();
    draft.steps[0].action = StepAction::Result {
        inputs: vec![Binding::Step {
            step: "result".into(),
            port: Port::Text,
        }],
    };
    let issues = o.validate_automation_draft(&draft).unwrap();
    assert_eq!(issues[0].step.as_deref(), Some(draft.steps[0].id.as_str()));
    assert_eq!(issues[0].field, "Inputs");
    let saved = o.save_automation("invalid", None, None, draft).unwrap();
    assert!(o
        .prepare_automation_activation(&saved.automation, 1)
        .await
        .is_err());
    assert!(o
        .admit_manual_automation_run(ManualStart {
            previous: None,
            automation: saved.automation.clone(),
            expected: 1,
            request: "invalid-run".into(),
            inputs: Default::default()
        })
        .await
        .is_err());
    assert!(o
        .work_page("person", false, None, 50)
        .unwrap()
        .entries
        .is_empty());
    let mut draft = sequence_draft();
    for index in 0..7 {
        draft.steps.push(Step {
            id: format!("extra{index}"),
            name: "Extra".into(),
            action: StepAction::Result { inputs: vec![] },
        });
    }
    assert_eq!(draft.steps.len(), 9);
    assert!(o.save_automation("too-many", None, None, draft).is_err());
    assert_eq!(o.automation_page(None, 10).unwrap().entries.len(), 1);
    let mut blank = sequence_draft();
    blank.name.clear();
    if let StepAction::Agent { prompt, .. } = &mut blank.steps[0].action {
        prompt.clear();
    }
    let saved = o
        .save_automation("blank", None, None, blank.clone())
        .unwrap();
    assert_eq!(
        o.automation(&saved.automation).unwrap().revision.draft.name,
        ""
    );
    let recovered = o
        .cancel_automation_save("blank", None, None, blank.clone())
        .unwrap()
        .unwrap();
    assert_eq!(recovered.automation, saved.automation);
    assert_eq!(
        o.save_automation("blank", None, None, blank.clone())
            .unwrap()
            .automation,
        saved.automation
    );
    assert_eq!(o.automation_page(None, 10).unwrap().entries.len(), 2);
    blank.name = "Name restored".into();
    let issues = o.validate_automation_draft(&blank).unwrap();
    assert_eq!(issues[0].field, "Task");
    assert_eq!(issues[0].step.as_deref(), Some(blank.steps[0].id.as_str()));
}

async fn wait_automation(
    o: &Orchestrator,
    id: &str,
    phase: crate::automation::RunPhase,
) -> crate::automation::Run {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let run = o.automation_run(id).unwrap();
            if run.phase == phase {
                return run;
            }
            assert!(
                !run.phase.is_final(),
                "unexpected final state {:?}: {:?}",
                run.phase,
                run.error
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|error| {
        panic!(
            "waiting for {phase:?}: {error}; current run: {:?}",
            o.automation_run(id)
        )
    })
}

#[tokio::test]
async fn cancelled_manual_start_fences_late_admission_and_recovers_existing_run() {
    use crate::automation::*;
    let (_dir, ark, project) = fixture();
    let o = Orchestrator::builder(&ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    let saved = o
        .save_automation("create", None, None, sequence_draft())
        .unwrap();
    let entry = o.automation(&saved.automation).unwrap();
    assert!(o
        .admit_automation_run(
            &saved.automation,
            1,
            RunOrigin::Manual {
                request: "unregistered".into()
            },
            Default::default()
        )
        .await
        .is_err());
    assert!(o
        .work_page("person", false, None, 50)
        .unwrap()
        .entries
        .is_empty());
    let intent = ManualStart {
        previous: None,
        automation: saved.automation.clone(),
        expected: 1,
        request: "lost-click".into(),
        inputs: Default::default(),
    };
    assert_eq!(o.begin_automation_start(intent.clone()).unwrap(), intent);
    let reopened = Orchestrator::builder(&ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    assert_eq!(
        reopened
            .automation(&saved.automation)
            .unwrap()
            .pending_start
            .unwrap(),
        intent
    );
    let competing = ManualStart {
        request: "another-client".into(),
        ..intent.clone()
    };
    assert_eq!(reopened.begin_automation_start(competing).unwrap(), intent);
    let prepared = o
        .inner
        .prepare_automation_choices(&entry.revision)
        .await
        .unwrap();
    let origin = RunOrigin::Manual {
        request: "lost-click".into(),
    };
    let inputs = std::collections::BTreeMap::new();
    let input_digest = origin.input_digest(&saved.automation, 1, &inputs).unwrap();
    for _ in 0..2 {
        assert!(o
            .cancel_automation_start(&saved.automation, 1, "lost-click", inputs.clone())
            .unwrap()
            .is_none());
    }
    assert!(reopened
        .automation(&saved.automation)
        .unwrap()
        .pending_start
        .is_none());
    assert!(o
        .inner
        .admit_frozen_automation_run(super::runs::FrozenRunAdmission {
            automation: &saved.automation,
            expected: 1,
            origin,
            inputs: inputs.clone(),
            input_digest,
            prepared,
            due_now: None,
            authenticated_connection: None,
        })
        .is_err());
    assert!(o
        .cancel_automation_start(&saved.automation, 2, "lost-click", inputs.clone())
        .is_err());
    assert!(o
        .work_page("person", false, None, 50)
        .unwrap()
        .entries
        .is_empty());
    let new_intent = ManualStart {
        request: "new-click".into(),
        ..intent
    };
    o.begin_automation_start(new_intent.clone()).unwrap();
    let run = o
        .admit_automation_run(
            &saved.automation,
            1,
            RunOrigin::Manual {
                request: "new-click".into(),
            },
            inputs.clone(),
        )
        .await
        .unwrap();
    reopened
        .acknowledge_automation_start(new_intent.clone())
        .unwrap();
    reopened
        .acknowledge_automation_start(new_intent.clone())
        .unwrap();
    assert_eq!(
        reopened
            .admit_manual_automation_run(new_intent)
            .await
            .unwrap()
            .id,
        run.id
    );
    assert!(o
        .automation(&saved.automation)
        .unwrap()
        .pending_start
        .is_none());
    let after_lost_ack = Orchestrator::builder(&ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    let receipt = after_lost_ack
        .automation(&saved.automation)
        .unwrap()
        .last_start
        .unwrap();
    assert_eq!(receipt.run, run.id);
    assert_eq!(receipt.work, run.work);
    let stale = ManualStart {
        request: "stale-client".into(),
        ..receipt.request.clone()
    };
    assert!(after_lost_ack
        .begin_automation_start(stale.clone())
        .is_err());
    let explicit_repeat = ManualStart {
        previous: Some(run.id.clone()),
        ..stale
    };
    assert_eq!(
        after_lost_ack
            .begin_automation_start(explicit_repeat.clone())
            .unwrap(),
        explicit_repeat
    );
    after_lost_ack
        .cancel_automation_start(&saved.automation, 1, "stale-client", Default::default())
        .unwrap();

    assert_eq!(
        o.cancel_automation_start(&saved.automation, 1, "new-click", inputs)
            .unwrap()
            .unwrap()
            .id,
        run.id
    );
    assert_eq!(
        o.work_page("person", false, None, 50)
            .unwrap()
            .entries
            .len(),
        1
    );
}

#[tokio::test]
async fn recovery_receipt_survives_losing_its_response_and_reopening_the_runtime() {
    use crate::automation::*;
    let (_dir, ark, project) = fixture();
    let o = Orchestrator::builder(&ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    let saved = o
        .save_automation("create", None, None, sequence_draft())
        .unwrap();
    let start = ManualStart {
        previous: None,
        automation: saved.automation.clone(),
        expected: 1,
        request: "recover-lost-reply".into(),
        inputs: Default::default(),
    };
    o.begin_automation_start(start.clone()).unwrap();
    let run = o
        .admit_automation_run(
            &saved.automation,
            1,
            RunOrigin::Manual {
                request: start.request.clone(),
            },
            Default::default(),
        )
        .await
        .unwrap();
    o.cancel_automation_start(&saved.automation, 1, &start.request, Default::default())
        .unwrap();
    drop(o);
    let reopened = Orchestrator::builder(&ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    let entry = reopened.automation(&saved.automation).unwrap();
    assert!(entry.pending_start.is_none());
    let receipt = entry.last_start.unwrap();
    assert_eq!(receipt.run, run.id);
    assert_eq!(receipt.work, run.work);
    assert!(reopened
        .begin_automation_start(ManualStart {
            request: "accidental-repeat".into(),
            ..start
        })
        .is_err());
    assert_eq!(
        reopened
            .work_page("person", false, None, 50)
            .unwrap()
            .entries
            .len(),
        1
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn automation_admission_is_atomic_deduplicated_and_approval_resumes_only_exact_question() {
    use crate::automation::*;
    let (_dir, ark, project) = fixture();
    let o = Orchestrator::builder(&ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    let mut draft = sequence_draft();
    draft.steps[0].action = StepAction::Approval {
        title: "Approve retained result".into(),
        inputs: vec![Binding::Literal {
            text: "EXACT_REVIEW".into(),
        }],
    };
    draft.steps[1].action = StepAction::Result {
        inputs: vec![Binding::Literal {
            text: "RETAINED_RESULT".into(),
        }],
    };
    let saved = o.save_automation("create", None, None, draft).unwrap();
    let origin = RunOrigin::Manual {
        request: "same-click".into(),
    };
    let (a, b) = tokio::join!(
        o.admit_confirmed_test_run(&saved.automation, 1, origin.clone(), Default::default()),
        o.admit_confirmed_test_run(&saved.automation, 1, origin.clone(), Default::default())
    );
    let run = a.unwrap();
    assert_eq!(run.id, b.unwrap().id);
    assert!(o.agents().is_empty());
    assert_eq!(
        o.work_page("person", false, None, 50)
            .unwrap()
            .entries
            .len(),
        1
    );
    assert_ne!(run.steps[0].scope, run.steps[1].scope);
    assert_ne!(run.steps[0].conversation, run.conversation);
    o.execute_automation_run(&run.id).unwrap();
    let waiting = wait_automation(&o, &run.id, RunPhase::WaitingApproval).await;
    let question = waiting.steps[0].question.as_ref().unwrap();
    assert_eq!(question.detail, "EXACT_REVIEW");
    assert!(o
        .answer_automation_question(&run.id, &question.id, "stale", ApprovalDecision::AllowOnce)
        .is_err());
    assert!(o
        .answer_automation_question(
            &run.id,
            &question.id,
            &question.digest,
            ApprovalDecision::AllowAlways
        )
        .is_err());
    o.answer_automation_question(
        &run.id,
        &question.id,
        &question.digest,
        ApprovalDecision::AllowOnce,
    )
    .unwrap();
    let completed = wait_automation(&o, &run.id, RunPhase::Completed).await;
    assert_eq!(completed.output.text, "RETAINED_RESULT");
    assert_eq!(
        o.work_detail("person", &run.work).unwrap().entries[0]
            .item
            .state,
        WorkState::Done
    );
    assert_eq!(
        o.admit_confirmed_test_run(&saved.automation, 1, origin, Default::default())
            .await
            .unwrap()
            .id,
        run.id
    );
    o.execute_automation_run(&run.id).unwrap();
    assert_eq!(
        Session::open(ark.engine(), &run.conversation)
            .unwrap()
            .unwrap()
            .turn_count(),
        2
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_automation_step_that_ends_on_a_note_after_narrated_calls_is_asked_once() {
    use crate::automation::*;
    use ark_model::{Completion, ToolCall};
    use ark_tools::{ActionOutput, Adapter, RunCtx};
    struct Look;
    #[async_trait::async_trait]
    impl Adapter for Look {
        fn name(&self) -> &str {
            "look"
        }
        fn actions(&self) -> Vec<ark_core::ActionDescriptor> {
            vec![ark_core::ActionDescriptor::new(
                "look.read",
                "read a file",
                ark_core::RiskTier::Read,
            )]
        }
        async fn run(
            &self,
            _: &str,
            _: serde_json::Value,
            _: &RunCtx<'_>,
        ) -> ark_tools::Result<ActionOutput> {
            Ok(ActionOutput::json(serde_json::json!({"text": "seed"})))
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let reply = |text: &str, calls: Vec<ToolCall>| Completion {
        content: text.into(),
        tool_calls: calls,
        usage: Default::default(),
    };
    let read = ToolCall {
        id: "c1".into(),
        name: "look.read".into(),
        arguments: serde_json::json!({}),
    };
    let provider = Arc::new(ark_model::MockProvider::new(
        "test",
        vec![
            reply("Reading the seed file.", vec![read]),
            reply("Checking the project files and verifying the seed.", vec![]),
            reply("Lead list: Acme, Birch, Cora.", vec![]),
        ],
    ));
    let ark = Ark::builder()
        .store_root(dir.path())
        .tool(Arc::new(Look))
        .provider(provider.clone())
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.create_project("project", "test").unwrap();
    let models = Arc::new(
        crate::Models::builder()
            .default_model("test")
            .provider("test", provider.clone())
            .build(),
    );
    let o = Orchestrator::builder(&ark, &project)
        .models(models)
        .build()
        .unwrap();
    let saved = o
        .save_automation("create", None, None, sequence_draft())
        .unwrap();
    let run = o
        .admit_confirmed_test_run(
            &saved.automation,
            1,
            RunOrigin::Manual {
                request: "run".into(),
            },
            Default::default(),
        )
        .await
        .unwrap();
    o.execute_automation_run(&run.id).unwrap();
    let completed = wait_automation(&o, &run.id, RunPhase::Completed).await;
    assert_eq!(completed.output.text, "Lead list: Acme, Birch, Cora.");
    assert_eq!(provider.calls.lock().unwrap().len(), 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn automation_step_context_only_contains_explicit_bindings_and_does_not_create_agents() {
    use crate::automation::*;
    let dir = tempfile::tempdir().unwrap();
    let reply = |text: &str| ark_model::Completion {
        content: text.into(),
        tool_calls: vec![],
        usage: Default::default(),
    };
    let provider = Arc::new(ark_model::MockProvider::new(
        "test",
        vec![reply("SECRET_FIRST_OUTPUT"), reply("SECOND_OUTPUT")],
    ));
    let ark = Ark::builder()
        .store_root(dir.path())
        .provider(provider.clone())
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.create_project("project", "test").unwrap();
    let models = Arc::new(
        crate::Models::builder()
            .default_model("test")
            .provider("test", provider.clone())
            .build(),
    );
    let o = Orchestrator::builder(&ark, &project)
        .models(models)
        .build()
        .unwrap();
    let mut draft = sequence_draft();
    draft.steps.insert(
        1,
        Step {
            id: "second".into(),
            name: "Second".into(),
            action: StepAction::Agent {
                target: WorkTarget::General {
                    model: Some("test".into()),
                },
                prompt: "Answer independently".into(),
                inputs: vec![],
            },
        },
    );
    draft.steps[2].action = StepAction::Result {
        inputs: vec![Binding::Step {
            step: "second".into(),
            port: Port::Text,
        }],
    };
    let saved = o.save_automation("create", None, None, draft).unwrap();
    let run = o
        .admit_confirmed_test_run(
            &saved.automation,
            1,
            RunOrigin::Manual {
                request: "run".into(),
            },
            Default::default(),
        )
        .await
        .unwrap();
    o.execute_automation_run(&run.id).unwrap();
    let completed = wait_automation(&o, &run.id, RunPhase::Completed).await;
    assert_eq!(completed.output.text, "SECOND_OUTPUT");
    assert!(o.agents().is_empty());
    let calls = provider.calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    let second = format!("{:?}", calls[1].messages);
    assert!(!second.contains("SECRET_FIRST_OUTPUT"), "{second}");
    assert_eq!(
        o.work_page("person", false, None, 50)
            .unwrap()
            .entries
            .len(),
        1
    );
    assert_eq!(
        Session::open(ark.engine(), &run.steps[0].conversation)
            .unwrap()
            .unwrap()
            .turn_count(),
        1
    );
    assert_eq!(
        Session::open(ark.engine(), &run.steps[1].conversation)
            .unwrap()
            .unwrap()
            .turn_count(),
        1
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn automation_existing_agent_reuses_task_executor_in_private_scope() {
    use crate::automation::*;
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ark_model::MockProvider::new(
        "test",
        vec![ark_model::Completion {
            content: "AGENT_STEP_RESULT".into(),
            tool_calls: vec![],
            usage: Default::default(),
        }],
    ));
    let ark = Ark::builder()
        .store_root(dir.path())
        .provider(provider.clone())
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.create_project("project", "test").unwrap();
    let models = Arc::new(
        crate::Models::builder()
            .default_model("test")
            .provider("test", provider.clone())
            .build(),
    );
    let o = Orchestrator::builder(&ark, &project)
        .models(models)
        .build()
        .unwrap();
    let agent = o
        .spawn(AgentSpec::new("reviewer").instructions("Review with care"))
        .await
        .unwrap();
    let mut draft = sequence_draft();
    draft.steps[0].action = StepAction::Agent {
        target: WorkTarget::Agent {
            name: agent.name.clone(),
        },
        prompt: "Reply once".into(),
        inputs: vec![],
    };
    let saved = o.save_automation("create", None, None, draft).unwrap();
    let run = o
        .admit_confirmed_test_run(
            &saved.automation,
            1,
            RunOrigin::Manual {
                request: "one".into(),
            },
            Default::default(),
        )
        .await
        .unwrap();
    o.execute_automation_run(&run.id).unwrap();
    let completed = wait_automation(&o, &run.id, RunPhase::Completed).await;
    assert_eq!(completed.output.text, "AGENT_STEP_RESULT");
    let task = o
        .task(completed.steps[0].receipt.as_deref().unwrap())
        .unwrap();
    assert_eq!(task.run.as_deref(), Some(run.id.as_str()));
    assert_eq!(
        task.execution_scope.as_deref(),
        Some(run.steps[0].scope.as_str())
    );
    assert_ne!(task.execution_scope.as_deref(), Some(agent.scope.as_str()));
    assert_eq!(o.agents().len(), 1);
    assert_eq!(provider.calls.lock().unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn automation_recovery_publishes_completed_step_without_repeating_model() {
    use crate::automation::*;
    let (_dir, ark, project) = fixture();
    let o = Orchestrator::builder(&ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    let saved = o
        .save_automation("create", None, None, sequence_draft())
        .unwrap();
    let mut run = o
        .admit_confirmed_test_run(
            &saved.automation,
            1,
            RunOrigin::Manual {
                request: "cut-after-result".into(),
            },
            Default::default(),
        )
        .await
        .unwrap();
    run.steps[0].phase = StepPhase::Completed;
    run.steps[0].output = Some(Output {
        text: "ALREADY_SAVED".into(),
        files: vec![],
    });
    run.phase = RunPhase::Running;
    run.owner = Some(RunOwner {
        token: "dead-owner".into(),
        pid: 999_999,
        pid_start: None,
    });
    ark.engine()
        .store
        .transact_records::<_, OrchError>(project.id.as_str(), |tx| {
            tx.put_record(RUN, &run.id, &serde_json::to_value(&run)?)?;
            Ok(())
        })
        .unwrap();
    o.execute_automation_run(&run.id).unwrap();
    let completed = wait_automation(&o, &run.id, RunPhase::Completed).await;
    assert_eq!(completed.output.text, "ALREADY_SAVED");
    let session = Session::open(ark.engine(), &run.conversation)
        .unwrap()
        .unwrap();
    assert_eq!(session.turn_count(), 2);
    assert_eq!(
        session.transcript_page(ark.engine(), None, 10).unwrap()[0].answer,
        "ALREADY_SAVED"
    );
}

#[tokio::test]
async fn automation_pending_cancel_updates_work_in_same_commit() {
    use crate::automation::*;
    let (_dir, _ark, project) = fixture();
    let o = Orchestrator::builder(&_ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    let saved = o
        .save_automation("create", None, None, sequence_draft())
        .unwrap();
    let run = o
        .admit_confirmed_test_run(
            &saved.automation,
            1,
            RunOrigin::Manual {
                request: "cancel-before-start".into(),
            },
            Default::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        o.cancel_automation_run(&run.id).unwrap().phase,
        RunPhase::Cancelled
    );
    assert_eq!(o.work_item(&run.work).unwrap().state, WorkState::Todo);
    assert_eq!(
        o.execute_automation_run(&run.id).unwrap().phase,
        RunPhase::Cancelled
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn automation_revoked_at_checkpoint_cannot_start_next_model() {
    use crate::automation::*;
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ark_model::MockProvider::new("test", vec![]));
    let ark = Ark::builder()
        .store_root(dir.path())
        .provider(provider.clone())
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.create_project("project", "test").unwrap();
    let models = Arc::new(
        crate::Models::builder()
            .default_model("test")
            .provider("test", provider.clone())
            .build(),
    );
    let o = Orchestrator::builder(&ark, &project)
        .models(models)
        .build()
        .unwrap();
    let mut draft = sequence_draft();
    draft.trigger = Trigger::Webhook;
    draft.steps.insert(
        0,
        Step {
            id: "approval".into(),
            name: "Approve".into(),
            action: StepAction::Approval {
                title: "Continue?".into(),
                inputs: vec![],
            },
        },
    );
    let saved = o.save_automation("create", None, None, draft).unwrap();
    let setup = o
        .configure_webhook(
            &saved.automation,
            1,
            "fixture-connection",
            "http://127.0.0.1:1/",
            60,
        )
        .unwrap();
    o.record_webhook_probe(&setup.endpoint, &setup.connection.id, true)
        .unwrap();
    let prepared = o
        .prepare_automation_activation(&saved.automation, 1)
        .await
        .unwrap();
    o.activate_automation(&saved.automation, &prepared.id, &prepared.digest)
        .unwrap();
    let run = o
        .admit_webhook_event(
            &setup.endpoint,
            &setup.connection.id,
            "event",
            now_ms().saturating_add(1),
            Default::default(),
        )
        .unwrap()
        .run;
    o.execute_automation_run(&run.id).unwrap();
    let waiting = wait_automation(&o, &run.id, RunPhase::WaitingApproval).await;
    o.revoke_automation_activation(&prepared.id, &prepared.digest)
        .unwrap();
    let question = waiting.steps[0].question.as_ref().unwrap();
    o.answer_automation_question(
        &run.id,
        &question.id,
        &question.digest,
        ApprovalDecision::AllowOnce,
    )
    .unwrap();
    let failed = wait_automation(&o, &run.id, RunPhase::Failed).await;
    assert!(failed.error.as_deref().unwrap().contains("revoked"));
    assert_eq!(provider.calls.lock().unwrap().len(), 0);
    assert_eq!(failed.steps[1].phase, StepPhase::Pending);
}

#[tokio::test(flavor = "multi_thread")]
async fn automation_answer_after_dead_owner_resumes_durable_checkpoint() {
    use crate::automation::*;
    let (_dir, ark, project) = fixture();
    let o = Orchestrator::builder(&ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    let mut draft = sequence_draft();
    draft.steps[0].action = StepAction::Approval {
        title: "Continue?".into(),
        inputs: vec![],
    };
    draft.steps[1].action = StepAction::Result {
        inputs: vec![Binding::Literal {
            text: "AFTER_RESTART".into(),
        }],
    };
    let saved = o.save_automation("create", None, None, draft).unwrap();
    let mut run = o
        .admit_confirmed_test_run(
            &saved.automation,
            1,
            RunOrigin::Manual {
                request: "restart".into(),
            },
            Default::default(),
        )
        .await
        .unwrap();
    run.phase = RunPhase::WaitingApproval;
    run.steps[0].phase = StepPhase::WaitingApproval;
    let question_digest = format!(
        "v1:{}",
        blake3::hash(
            &serde_json::to_vec(&(&run.revision.digest, &run.steps[0].id, Output::default()))
                .unwrap()
        )
        .to_hex()
    );
    run.steps[0].question = Some(RunQuestion {
        id: "persisted-question".into(),
        digest: question_digest.clone(),
        title: "Continue?".into(),
        detail: String::new(),
        decision: None,
        action: None,
        files: vec![],
    });
    assert!(!process::writer_alive(999_999, None));
    run.owner = Some(RunOwner {
        token: "dead".into(),
        pid: 999_999,
        pid_start: None,
    });
    ark.engine()
        .store
        .transact_records::<_, OrchError>(project.id.as_str(), |tx| {
            tx.put_record(RUN, &run.id, &serde_json::to_value(&run)?)?;
            Ok(())
        })
        .unwrap();
    o.answer_automation_question(
        &run.id,
        "persisted-question",
        &question_digest,
        ApprovalDecision::AllowOnce,
    )
    .unwrap();
    assert_eq!(
        wait_automation(&o, &run.id, RunPhase::Completed)
            .await
            .output
            .text,
        "AFTER_RESTART"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn automation_schedule_admits_revision_and_advances_occurrence_atomically_across_instances() {
    use crate::automation::*;
    let (_dir, ark, project) = fixture();
    let o = Orchestrator::builder(&ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    let observer = Orchestrator::builder(&ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    let mut draft = sequence_draft();
    draft.trigger = Trigger::Schedule {
        when: When::every_minutes(5),
    };
    draft.steps = vec![Step {
        id: "result".into(),
        name: "Result".into(),
        action: StepAction::Result {
            inputs: vec![Binding::Literal {
                text: "SCHEDULED_RESULT".into(),
            }],
        },
    }];
    let saved = o
        .save_automation("create", None, None, draft.clone())
        .unwrap();
    let prepared = o
        .prepare_automation_activation(&saved.automation, 1)
        .await
        .unwrap();
    let head = o
        .activate_automation(&saved.automation, &prepared.id, &prepared.digest)
        .unwrap();
    let schedule_id = head.schedule.unwrap();
    let record = o.schedule_record(&schedule_id).unwrap();
    let slot = record.next_ms.unwrap();
    assert_eq!(record.target.as_deref(), Some(saved.automation.as_str()));
    let runs = observer.tick_schedules(slot).await;
    assert_eq!(runs.len(), 1);
    let completed = wait_automation(&observer, &runs[0], RunPhase::Completed).await;
    assert_eq!(completed.output.text, "SCHEDULED_RESULT");
    assert_eq!(completed.revision.number, 1);
    assert!(o.tick_schedules(slot).await.is_empty());
    let next = o.schedule_record(&schedule_id).unwrap();
    assert_eq!(next.runs, 1);
    assert!(next.next_ms.unwrap() > slot);
    assert_eq!(next.last_task.as_deref(), Some(runs[0].as_str()));
    draft.trigger = Trigger::Manual;
    let changed = o
        .save_automation("draft-two", Some(&saved.automation), Some(1), draft)
        .unwrap();
    assert_eq!(changed.revision, 2);
    let second = o.tick_schedules(next.next_ms.unwrap()).await;
    assert_eq!(second.len(), 1);
    assert_eq!(
        wait_automation(&o, &second[0], RunPhase::Completed)
            .await
            .revision
            .number,
        1
    );
    let prepared = o
        .prepare_automation_activation(&saved.automation, 2)
        .await
        .unwrap();
    o.activate_automation(&saved.automation, &prepared.id, &prepared.digest)
        .unwrap();
    assert!(observer
        .tick_schedules(next.next_ms.unwrap() + 600_000)
        .await
        .is_empty());
    let stopped = observer.schedule_record(&schedule_id).unwrap();
    assert!(stopped.paused);
    assert!(stopped.next_ms.is_none());
    assert_eq!(stopped.runs, 2);
    assert!(observer.run_schedule_now(&schedule_id).await.is_err());
    assert_eq!(
        o.work_page("person", false, None, 50)
            .unwrap()
            .entries
            .len(),
        2
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn stale_legacy_schedule_dispatch_cannot_cross_adopted_target() {
    let (_dir, ark, project) = fixture();
    let o = Orchestrator::builder(&ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    o.spawn(AgentSpec::new("worker")).await.unwrap();
    let mut record = o
        .schedule("worker", TaskSpec::new("old work"), When::every_minutes(5))
        .unwrap();
    ark.engine()
        .store
        .transact_records::<_, OrchError>(project.id.as_str(), |tx| {
            let mut changed = record.clone();
            changed.target = Some("adopted-definition".into());
            changed.version += 1;
            tx.put_record(SCHEDULE, &changed.id, &serde_json::to_value(&changed)?)?;
            Ok(())
        })
        .unwrap();
    let slot = record.next_ms.unwrap();
    assert!(o
        .inner
        .start_run(&mut record, slot, slot, true, None)
        .await
        .is_err());
    assert!(o.tasks().is_empty());
    let durable: ScheduleRecord = serde_json::from_value(
        ark.engine()
            .store
            .get_record(SCHEDULE, &record.id)
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(durable.next_ms, Some(slot));
    assert_eq!(durable.runs, 0);
    assert_eq!(durable.target.as_deref(), Some("adopted-definition"));
}

#[tokio::test(flavor = "multi_thread")]
async fn automation_schedule_rollback_keeps_occurrence_and_leaves_no_partial_work_or_sessions() {
    use crate::automation::*;
    let (dir, ark, project) = fixture();
    let o = Orchestrator::builder(&ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    let mut draft = sequence_draft();
    draft.trigger = Trigger::Schedule {
        when: When::every_minutes(5),
    };
    draft.steps = vec![Step {
        id: "result".into(),
        name: "Result".into(),
        action: StepAction::Result {
            inputs: vec![Binding::Literal {
                text: "atomic".into(),
            }],
        },
    }];
    let saved = o.save_automation("create", None, None, draft).unwrap();
    let grant = o
        .prepare_automation_activation(&saved.automation, 1)
        .await
        .unwrap();
    let head = o
        .activate_automation(&saved.automation, &grant.id, &grant.digest)
        .unwrap();
    let schedule = o
        .schedule_record(head.schedule.as_deref().unwrap())
        .unwrap();
    let slot = schedule.next_ms.unwrap();
    let connection = rusqlite::Connection::open(dir.path().join("meta.db")).unwrap();
    let count = |table: &str| {
        connection
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get::<_, u64>(0)
            })
            .unwrap()
    };
    let before = (count("projects"), count("sessions"), count("records"));
    connection.execute_batch("CREATE TRIGGER refuse_sequence_run BEFORE INSERT ON records WHEN NEW.kind='automation_run' BEGIN SELECT RAISE(ABORT,'injected run storage failure'); END;").unwrap();
    assert!(o.tick_schedules(slot).await.is_empty());
    assert_eq!(
        (count("projects"), count("sessions"), count("records")),
        before
    );
    let reopened = Orchestrator::builder(&ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    let retained = reopened.schedule_record(&schedule.id).unwrap();
    assert_eq!(retained.next_ms, Some(slot));
    assert_eq!(retained.runs, 0);
    assert!(retained.last_task.is_none());
    assert!(reopened
        .work_page("person", false, None, 50)
        .unwrap()
        .entries
        .is_empty());
    connection
        .execute_batch("DROP TRIGGER refuse_sequence_run")
        .unwrap();
    let runs = reopened.tick_schedules(slot).await;
    assert_eq!(runs.len(), 1);
    wait_automation(&reopened, &runs[0], RunPhase::Completed).await;
    assert_eq!(reopened.schedule_record(&schedule.id).unwrap().runs, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn automation_schedule_cutover_skips_unadmitted_old_slot_with_same_when() {
    use crate::automation::*;
    let (_dir, ark, project) = fixture();
    let o = Orchestrator::builder(&ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    let mut draft = sequence_draft();
    draft.trigger = Trigger::Schedule {
        when: When::every_minutes(5),
    };
    draft.steps = vec![Step {
        id: "result".into(),
        name: "Result".into(),
        action: StepAction::Result {
            inputs: vec![Binding::Literal {
                text: "revision one".into(),
            }],
        },
    }];
    let saved = o
        .save_automation("create", None, None, draft.clone())
        .unwrap();
    let first = o
        .prepare_automation_activation(&saved.automation, 1)
        .await
        .unwrap();
    let head = o
        .activate_automation(&saved.automation, &first.id, &first.digest)
        .unwrap();
    let schedule_id = head.schedule.unwrap();
    let old_slot = now_ms() - 1000;
    ark.engine()
        .store
        .transact_records::<_, OrchError>(project.id.as_str(), |tx| {
            let mut record: ScheduleRecord =
                serde_json::from_value(tx.get_record(SCHEDULE, &schedule_id)?.unwrap())?;
            record.next_ms = Some(old_slot);
            record.target_since_ms = old_slot - 300_000;
            tx.put_record(SCHEDULE, &schedule_id, &serde_json::to_value(&record)?)?;
            Ok(())
        })
        .unwrap();
    draft.steps[0].action = StepAction::Result {
        inputs: vec![Binding::Literal {
            text: "revision two".into(),
        }],
    };
    o.save_automation("edit", Some(&saved.automation), Some(1), draft)
        .unwrap();
    let second = o
        .prepare_automation_activation(&saved.automation, 2)
        .await
        .unwrap();
    o.activate_automation(&saved.automation, &second.id, &second.digest)
        .unwrap();
    assert!(o.tick_schedules(now_ms()).await.is_empty());
    let after = o.schedule_record(&schedule_id).unwrap();
    assert_eq!(after.runs, 0);
    assert_eq!(after.skipped, 1);
    assert!(after.next_ms.unwrap() > after.target_since_ms);
    let runs = o.tick_schedules(after.next_ms.unwrap()).await;
    assert_eq!(runs.len(), 1);
    let completed = wait_automation(&o, &runs[0], RunPhase::Completed).await;
    assert_eq!(completed.revision.number, 2);
    assert_eq!(completed.output.text, "revision two");
}

#[tokio::test(flavor = "multi_thread")]
async fn automation_schedule_run_now_is_idempotent_and_never_moves_next_occurrence() {
    use crate::automation::*;
    let (_dir, ark, project) = fixture();
    let o = Orchestrator::builder(&ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    let mut draft = sequence_draft();
    draft.trigger = Trigger::Schedule {
        when: When::every_minutes(5),
    };
    draft.steps = vec![Step {
        id: "result".into(),
        name: "Result".into(),
        action: StepAction::Result {
            inputs: vec![Binding::Literal {
                text: "manual result".into(),
            }],
        },
    }];
    let saved = o
        .save_automation("create", None, None, draft.clone())
        .unwrap();
    let grant = o
        .prepare_automation_activation(&saved.automation, 1)
        .await
        .unwrap();
    let head = o
        .activate_automation(&saved.automation, &grant.id, &grant.digest)
        .unwrap();
    let schedule_id = head.schedule.unwrap();
    let next = o.schedule_record(&schedule_id).unwrap().next_ms;
    let id = o
        .run_schedule_now_with_key(&schedule_id, "one click")
        .await
        .unwrap();
    wait_automation(&o, &id, RunPhase::Completed).await;
    assert_eq!(o.schedule_record(&schedule_id).unwrap().next_ms, next);
    assert_eq!(
        o.run_schedule_now_with_key(&schedule_id, "one click")
            .await
            .unwrap(),
        id
    );
    o.pause_automation(&saved.automation).unwrap();
    let paused = o.schedule_record(&schedule_id).unwrap();
    let other = o
        .run_schedule_now_with_key(&schedule_id, "run while paused")
        .await
        .unwrap();
    wait_automation(&o, &other, RunPhase::Completed).await;
    let after = o.schedule_record(&schedule_id).unwrap();
    assert!(after.paused);
    assert_eq!(after.next_ms, paused.next_ms);
    draft.trigger = Trigger::Manual;
    o.save_automation("edit", Some(&saved.automation), Some(1), draft)
        .unwrap();
    let new_grant = o
        .prepare_automation_activation(&saved.automation, 2)
        .await
        .unwrap();
    o.activate_automation(&saved.automation, &new_grant.id, &new_grant.digest)
        .unwrap();
    assert_eq!(
        o.run_schedule_now_with_key(&schedule_id, "one click")
            .await
            .unwrap(),
        id
    );
    assert!(o
        .run_schedule_now_with_key(&schedule_id, "new click")
        .await
        .is_err());
    assert_eq!(
        o.work_page("person", false, None, 50)
            .unwrap()
            .entries
            .len(),
        2
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn legacy_schedule_run_now_key_is_shared_across_concurrent_instances_and_restart() {
    let (_dir, ark, project) = fixture();
    let o = Orchestrator::builder(&ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    o.spawn(AgentSpec::new("worker")).await.unwrap();
    let schedule = o
        .schedule_with(
            "worker",
            TaskSpec::new("legacy run"),
            When::every_minutes(5),
            ScheduleChange {
                overlap: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
    let other = Orchestrator::builder(&ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let left = o.clone();
    let right = other.clone();
    let one_id = schedule.id.clone();
    let two_id = one_id.clone();
    let b1 = barrier.clone();
    let b2 = barrier;
    let h1 = tokio::runtime::Handle::current();
    let h2 = h1.clone();
    let first = std::thread::spawn(move || {
        let _entered = h1.enter();
        b1.wait();
        h1.block_on(left.run_schedule_now_with_key(&one_id, "same click"))
    });
    let second = std::thread::spawn(move || {
        let _entered = h2.enter();
        b2.wait();
        h2.block_on(right.run_schedule_now_with_key(&two_id, "same click"))
    });
    let id = first.join().unwrap().unwrap();
    assert_eq!(id, second.join().unwrap().unwrap());
    let owner = if o.task(&id).is_some() { &o } else { &other };
    tokio::time::timeout(Duration::from_secs(5), owner.wait(&id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        o.run_schedule_now_with_key(&schedule.id, "same click")
            .await
            .unwrap(),
        id
    );
    let reopened = Orchestrator::builder(&ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    assert_eq!(
        reopened
            .run_schedule_now_with_key(&schedule.id, "same click")
            .await
            .unwrap(),
        id
    );
    let receipt = reopened
        .schedule_run_receipt(&schedule.id, "same click")
        .unwrap()
        .unwrap();
    assert_eq!(receipt.request, "same click");
    assert_eq!(receipt.kind, crate::ScheduleExecutionKind::Task);
    assert_eq!(
        reopened.schedule_record(&schedule.id).unwrap().next_ms,
        schedule.next_ms
    );
    assert_eq!(reopened.schedule_record(&schedule.id).unwrap().runs, 1);
    assert_eq!(reopened.tasks().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn activation_cutover_rejects_an_occurrence_at_the_exact_watermark() {
    use crate::automation::*;
    let (_dir, ark, project) = fixture();
    let o = Orchestrator::builder(&ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    let mut draft = sequence_draft();
    draft.trigger = Trigger::Schedule {
        when: When::every_minutes(5),
    };
    draft.steps = vec![Step {
        id: "result".into(),
        name: "Result".into(),
        action: StepAction::Result { inputs: vec![] },
    }];
    let saved = o.save_automation("create", None, None, draft).unwrap();
    let grant = o
        .prepare_automation_activation(&saved.automation, 1)
        .await
        .unwrap();
    let head = o
        .activate_automation(&saved.automation, &grant.id, &grant.digest)
        .unwrap();
    let schedule_id = head.schedule.unwrap();
    let watermark = o.schedule_record(&schedule_id).unwrap().target_since_ms;
    ark.engine()
        .store
        .transact_records::<_, OrchError>(project.id.as_str(), |tx| {
            let mut record: ScheduleRecord =
                serde_json::from_value(tx.get_record(SCHEDULE, &schedule_id)?.unwrap())?;
            record.next_ms = Some(watermark);
            tx.put_record(SCHEDULE, &schedule_id, &serde_json::to_value(record)?)?;
            Ok(())
        })
        .unwrap();
    assert!(o.tick_schedules(watermark).await.is_empty());
    let after = o.schedule_record(&schedule_id).unwrap();
    assert_eq!(after.runs, 0);
    assert_eq!(after.skipped, 1);
    assert!(after.next_ms.unwrap() > watermark);
}

#[tokio::test(flavor = "multi_thread")]
async fn webhook_policy_keeps_endpoint_identity_through_draft_connection_and_rotation() {
    use crate::automation::*;
    use std::collections::BTreeMap;
    let (_dir, ark, project) = fixture();
    let o = Orchestrator::builder(&ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    let mut draft = sequence_draft();
    draft.trigger = Trigger::Webhook;
    draft.inputs = vec![Input {
        name: "message".into(),
        port: Port::Text,
        required: true,
    }];
    draft.steps = vec![Step {
        id: "result".into(),
        name: "Result".into(),
        action: StepAction::Result {
            inputs: vec![Binding::Input {
                name: "message".into(),
                port: Port::Text,
            }],
        },
    }];
    let saved = o
        .save_automation("create", None, None, draft.clone())
        .unwrap();
    assert!(o
        .prepare_automation_activation(&saved.automation, 1)
        .await
        .is_err());
    let setup = o
        .configure_webhook(
            &saved.automation,
            1,
            "connection-one",
            "http://127.0.0.1:1/",
            2,
        )
        .unwrap();
    assert_eq!(
        o.configure_webhook(
            &saved.automation,
            1,
            "connection-one",
            "http://127.0.0.1:1/",
            2
        )
        .unwrap()
        .connection
        .id,
        setup.connection.id
    );
    assert!(o
        .configure_webhook(
            &saved.automation,
            1,
            "connection-one",
            "http://127.0.0.1:2/",
            2
        )
        .is_err());
    assert!(o
        .prepare_automation_activation(&saved.automation, 1)
        .await
        .is_err());
    o.record_webhook_probe(&setup.endpoint, &setup.connection.id, true)
        .unwrap();
    let grant = o
        .prepare_automation_activation(&saved.automation, 1)
        .await
        .unwrap();
    assert_eq!(
        grant.webhook.as_ref().unwrap().connection,
        setup.connection.id
    );
    o.activate_automation(&saved.automation, &grant.id, &grant.digest)
        .unwrap();
    assert_eq!(
        o.automation(&saved.automation)
            .unwrap()
            .webhook_availability,
        Some(WebhookAvailability::Local)
    );
    assert_eq!(
        o.automation_page(None, 50).unwrap().entries[0].webhook_availability,
        Some(WebhookAvailability::Local)
    );
    let inputs = BTreeMap::from([(
        "message".into(),
        InputValue::Text {
            value: "original input".into(),
        },
    )]);
    let first = o
        .admit_webhook_event(
            &setup.endpoint,
            &setup.connection.id,
            "event-one",
            now_ms().saturating_add(1),
            inputs.clone(),
        )
        .unwrap();
    assert!(!first.duplicate);
    let replacement = o
        .configure_webhook(
            &saved.automation,
            1,
            "connection-two",
            "http://127.0.0.1:2/",
            2,
        )
        .unwrap();
    assert_eq!(replacement.endpoint, setup.endpoint);
    assert_ne!(replacement.connection.id, setup.connection.id);
    assert_eq!(
        o.webhook_endpoint(&setup.endpoint)
            .unwrap()
            .active
            .unwrap()
            .id,
        setup.connection.id
    );
    assert!(o
        .admit_webhook_event(
            &setup.endpoint,
            &replacement.connection.id,
            "new-credential-before-activation",
            now_ms().saturating_add(1),
            inputs.clone()
        )
        .is_err());
    assert!(
        o.admit_webhook_event(
            &setup.endpoint,
            &setup.connection.id,
            "event-one",
            now_ms().saturating_add(1),
            inputs.clone()
        )
        .unwrap()
        .duplicate
    );
    o.record_webhook_probe(&replacement.endpoint, &replacement.connection.id, true)
        .unwrap();
    let replacement_grant = o
        .prepare_automation_activation(&saved.automation, 1)
        .await
        .unwrap();
    o.activate_automation(
        &saved.automation,
        &replacement_grant.id,
        &replacement_grant.digest,
    )
    .unwrap();
    let replay = o
        .admit_webhook_event(
            &replacement.endpoint,
            &replacement.connection.id,
            "event-one",
            now_ms().saturating_add(1),
            inputs.clone(),
        )
        .unwrap();
    assert!(replay.duplicate);
    assert_eq!(replay.run.id, first.run.id);
    assert_eq!(replay.run.activation, first.run.activation);
    assert!(o
        .admit_webhook_event(
            &setup.endpoint,
            &setup.connection.id,
            "event-one",
            now_ms().saturating_add(1),
            inputs.clone()
        )
        .is_err());
    let changed = BTreeMap::from([(
        "message".into(),
        InputValue::Text {
            value: "changed input".into(),
        },
    )]);
    assert!(o
        .admit_webhook_event(
            &replacement.endpoint,
            &replacement.connection.id,
            "event-one",
            now_ms().saturating_add(1),
            changed
        )
        .is_err());
    o.admit_webhook_event(
        &replacement.endpoint,
        &replacement.connection.id,
        "event-two",
        now_ms().saturating_add(1),
        inputs.clone(),
    )
    .unwrap();
    assert!(o
        .admit_webhook_event(
            &replacement.endpoint,
            &replacement.connection.id,
            "event-three",
            now_ms().saturating_add(1),
            inputs.clone()
        )
        .is_err());
    o.pause_automation(&saved.automation).unwrap();
    assert_eq!(
        o.automation(&saved.automation)
            .unwrap()
            .webhook_availability,
        None
    );
    assert!(
        o.admit_webhook_event(
            &replacement.endpoint,
            &replacement.connection.id,
            "event-one",
            now_ms().saturating_add(1),
            inputs.clone()
        )
        .unwrap()
        .duplicate
    );
    assert!(o
        .admit_webhook_event(
            &replacement.endpoint,
            &replacement.connection.id,
            "paused-new-event",
            now_ms().saturating_add(1),
            inputs.clone()
        )
        .is_err());
    draft.trigger = Trigger::Manual;
    o.save_automation("manual", Some(&saved.automation), Some(1), draft)
        .unwrap();
    let manual = o
        .prepare_automation_activation(&saved.automation, 2)
        .await
        .unwrap();
    o.activate_automation(&saved.automation, &manual.id, &manual.digest)
        .unwrap();
    assert_eq!(
        o.admit_webhook_event(
            &replacement.endpoint,
            &replacement.connection.id,
            "event-one",
            now_ms().saturating_add(1),
            inputs
        )
        .unwrap()
        .run
        .revision
        .number,
        1
    );
    assert_eq!(
        o.work_page("person", false, None, 50)
            .unwrap()
            .entries
            .len(),
        2
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn webhook_policy_quota_failure_does_not_create_a_partial_execution() {
    use crate::automation::*;
    let (dir, ark, project) = fixture();
    let o = Orchestrator::builder(&ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    let mut draft = sequence_draft();
    draft.trigger = Trigger::Webhook;
    let saved = o.save_automation("create", None, None, draft).unwrap();
    let setup = o
        .configure_webhook(
            &saved.automation,
            1,
            "connection",
            "http://127.0.0.1:1/",
            60,
        )
        .unwrap();
    o.record_webhook_probe(&setup.endpoint, &setup.connection.id, true)
        .unwrap();
    let grant = o
        .prepare_automation_activation(&saved.automation, 1)
        .await
        .unwrap();
    o.activate_automation(&saved.automation, &grant.id, &grant.digest)
        .unwrap();
    let connection = rusqlite::Connection::open(dir.path().join("meta.db")).unwrap();
    connection
        .execute_batch("DROP TABLE quota_windows")
        .unwrap();
    assert!(o
        .admit_webhook_event(
            &setup.endpoint,
            &setup.connection.id,
            "event-one",
            now_ms().saturating_add(1),
            Default::default()
        )
        .is_err());
    assert!(o
        .work_page("person", false, None, 50)
        .unwrap()
        .entries
        .is_empty());
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM sessions", [], |row| row
                .get::<_, u64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        o.automation_history(&saved.automation, None, 20)
            .unwrap()
            .runs
            .len(),
        0
    );
}

#[tokio::test]
async fn webhook_cutover_rejects_unadmitted_old_events_but_preserves_receipts() {
    use crate::automation::*;
    let (_dir, ark, project) = fixture();
    let o = Orchestrator::builder(&ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    let mut draft = sequence_draft();
    draft.trigger = Trigger::Webhook;
    draft.steps = vec![Step {
        id: "result".into(),
        name: "Result".into(),
        action: StepAction::Result { inputs: vec![] },
    }];
    let saved = o
        .save_automation("create", None, None, draft.clone())
        .unwrap();
    let setup = o
        .configure_webhook(
            &saved.automation,
            1,
            "connection",
            "http://127.0.0.1:1/",
            60,
        )
        .unwrap();
    o.record_webhook_probe(&setup.endpoint, &setup.connection.id, true)
        .unwrap();
    let grant = o
        .prepare_automation_activation(&saved.automation, 1)
        .await
        .unwrap();
    o.activate_automation(&saved.automation, &grant.id, &grant.digest)
        .unwrap();
    let before = now_ms().saturating_add(1);
    let original = o
        .admit_webhook_event(
            &setup.endpoint,
            &setup.connection.id,
            "admitted",
            before,
            Default::default(),
        )
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    draft.name = "New revision".into();
    o.save_automation("edit", Some(&saved.automation), Some(1), draft)
        .unwrap();
    let grant = o
        .prepare_automation_activation(&saved.automation, 2)
        .await
        .unwrap();
    o.activate_automation(&saved.automation, &grant.id, &grant.digest)
        .unwrap();
    assert!(o
        .admit_webhook_event(
            &setup.endpoint,
            &setup.connection.id,
            "old-unadmitted",
            before,
            Default::default()
        )
        .err()
        .unwrap()
        .to_string()
        .contains("predates"));
    let replay = o
        .admit_webhook_event(
            &setup.endpoint,
            &setup.connection.id,
            "admitted",
            before,
            Default::default(),
        )
        .unwrap();
    assert!(replay.duplicate);
    assert_eq!(replay.run.id, original.run.id);
    assert_eq!(replay.run.revision.number, 1);
    let fresh = o
        .admit_webhook_event(
            &setup.endpoint,
            &setup.connection.id,
            "new",
            now_ms().saturating_add(1),
            Default::default(),
        )
        .unwrap();
    assert_eq!(fresh.run.revision.number, 2);
}

#[tokio::test]
async fn automation_recovery_restores_journaled_answer_after_owner_dies() {
    use crate::automation::*;
    for transcript_saved in [false, true] {
        let (_dir, ark, project) = fixture();
        let provider = Arc::new(ark_model::MockProvider::new("test", vec![]));
        let o = Orchestrator::builder(&ark, &project)
            .models(Arc::new(
                crate::Models::builder()
                    .default_model("test")
                    .provider("test", provider.clone())
                    .build(),
            ))
            .build()
            .unwrap();
        let saved = o
            .save_automation("create", None, None, sequence_draft())
            .unwrap();
        let mut run = o
            .admit_confirmed_test_run(
                &saved.automation,
                1,
                RunOrigin::Manual {
                    request: "journal-boundary".into(),
                },
                Default::default(),
            )
            .await
            .unwrap();
        let mut session = Session::open(ark.engine(), &run.steps[0].conversation)
            .unwrap()
            .unwrap();
        let outcome = ark_runtime::HarnessOutcome {
            answer: "RETAINED_AFTER_CRASH".into(),
            cited: vec![],
            steps: 1,
            trivial: false,
            metrics: Default::default(),
            state: Default::default(),
            cancelled: false,
            interrupted: false,
            steered: vec![],
            ran_out: false,
        };
        let input = "Review the supplied text";
        let result = results::SavedResult::capture(&session, input, &outcome);
        run.steps[0].phase = StepPhase::Running;
        run.phase = RunPhase::Interrupted;
        run.owner = Some(RunOwner {
            token: "dead-owner".into(),
            pid: 999_999,
            pid_start: None,
        });
        ark.engine()
            .store
            .transact_records::<_, OrchError>(project.id.as_str(), |tx| {
                tx.put_record(RUN, &run.id, &serde_json::to_value(&run)?)?;
                tx.insert_record(
                    "automation_turn_result",
                    &format!("{}/{}", run.id, run.steps[0].id),
                    &serde_json::to_value(result)?,
                )?;
                Ok(())
            })
            .unwrap();
        if transcript_saved {
            session.record_turn(ark.engine(), input, &outcome).unwrap();
        }
        o.execute_automation_run(&run.id).unwrap();
        let completed = wait_automation(&o, &run.id, RunPhase::Completed).await;
        assert_eq!(completed.output.text, "RETAINED_AFTER_CRASH");
        assert!(provider.calls.lock().unwrap().is_empty());
        assert_eq!(
            Session::open(ark.engine(), &run.steps[0].conversation)
                .unwrap()
                .unwrap()
                .turn_count(),
            1
        );
        assert_eq!(
            Session::open(ark.engine(), &run.conversation)
                .unwrap()
                .unwrap()
                .turn_count(),
            2
        );
    }
}

#[tokio::test]
async fn automation_refuses_uncertified_provider_before_admission_or_any_model_call() {
    struct Uncertified(Arc<std::sync::atomic::AtomicUsize>);
    #[async_trait::async_trait]
    impl ark_model::Provider for Uncertified {
        async fn complete(
            &self,
            _: &str,
            _: &[ark_model::Message],
            _: &[ark_model::ToolDef],
        ) -> ark_model::Result<ark_model::Completion> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            unreachable!("an uncertified provider must not be called")
        }
        fn describe(&self) -> String {
            "uncertified".into()
        }
    }
    use crate::automation::*;
    let (_dir, ark, project) = fixture();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let provider = Arc::new(Uncertified(calls.clone()));
    let o = Orchestrator::builder(&ark, &project)
        .models(Arc::new(
            crate::Models::builder()
                .default_model("uncertified")
                .provider("uncertified", provider)
                .build(),
        ))
        .build()
        .unwrap();
    let mut draft = sequence_draft();
    if let StepAction::Agent { target, .. } = &mut draft.steps[0].action {
        *target = WorkTarget::General {
            model: Some("uncertified".into()),
        };
    }
    let saved = o.save_automation("create", None, None, draft).unwrap();
    assert!(o
        .admit_confirmed_test_run(
            &saved.automation,
            1,
            RunOrigin::Manual {
                request: "denied".into()
            },
            Default::default()
        )
        .await
        .is_err());
    assert!(o
        .prepare_automation_activation(&saved.automation, 1)
        .await
        .is_err());
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert!(o
        .automation_history(&saved.automation, None, 20)
        .unwrap()
        .runs
        .is_empty());
    assert!(o
        .work_page("person", false, None, 20)
        .unwrap()
        .entries
        .is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn automation_delegation_from_general_and_agent_keeps_frozen_work_and_capacity() {
    use crate::automation::*;
    use ark_model::{Completion, ToolCall};
    for agent_parent in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let parent = Arc::new(ark_model::MockProvider::new(
            "parent",
            vec![
                Completion {
                    content: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "delegate-one".into(),
                        name: tools::DELEGATE.into(),
                        arguments: serde_json::json!({"to":"child","objective":"Only this delegated brief","wait":true}),
                    }],
                    usage: Default::default(),
                },
                Completion {
                    content: "PARENT_FINISHED".into(),
                    tool_calls: vec![],
                    usage: Default::default(),
                },
            ],
        ));
        let child = Arc::new(ark_model::MockProvider::new(
            "child",
            vec![Completion {
                content: "CHILD_PRIVATE_OUTPUT".into(),
                tool_calls: vec![],
                usage: Default::default(),
            }],
        ));
        let profile = AgentProfile {
            core_actions: vec![tools::DELEGATE.into()],
            ..Default::default()
        };
        let ark = Ark::builder()
            .store_root(dir.path())
            .provider(parent.clone())
            .profile(profile)
            .planner(false)
            .critic(false)
            .build()
            .unwrap();
        let project = ark.create_project("delegation test", "").unwrap();
        let o = Orchestrator::builder(&ark, &project)
            .max_parallel(1)
            .models(Arc::new(
                crate::Models::builder()
                    .default_model("parent")
                    .provider("parent", parent.clone())
                    .provider("child-model", child.clone())
                    .capacity("parent", ark_model::capacity::Capacity::concurrent(1))
                    .capacity("child-model", ark_model::capacity::Capacity::concurrent(1))
                    .build(),
            ))
            .build()
            .unwrap();
        let child_agent = o
            .spawn(AgentSpec::new("child").model("child-model"))
            .await
            .unwrap();
        let parent_agent = if agent_parent {
            Some(
                o.spawn(
                    AgentSpec::new("parent")
                        .model("parent")
                        .delegates_to(["child"]),
                )
                .await
                .unwrap(),
            )
        } else {
            None
        };
        let mut draft = sequence_draft();
        if let StepAction::Agent { target, .. } = &mut draft.steps[0].action {
            *target = parent_agent
                .as_ref()
                .map(|agent| WorkTarget::Agent {
                    name: agent.name.clone(),
                })
                .unwrap_or(WorkTarget::General {
                    model: Some("parent".into()),
                });
        }
        draft.authority.actions = vec![AllowedAction {
            action: tools::DELEGATE.into(),
            inputs: std::collections::BTreeMap::from([
                (
                    "to".into(),
                    Bound::Literal {
                        value: serde_json::json!("child"),
                    },
                ),
                ("objective".into(), Bound::Free),
                (
                    "wait".into(),
                    Bound::Literal {
                        value: serde_json::json!(true),
                    },
                ),
            ]),
            unattended: false,
            per_run: 2,
            quota: Quota {
                count: 10,
                window_ms: 3_600_000,
            },
        }];
        let saved = o.save_automation("create", None, None, draft).unwrap();
        let run = o
            .admit_confirmed_test_run(
                &saved.automation,
                1,
                RunOrigin::Manual {
                    request: "one".into(),
                },
                Default::default(),
            )
            .await
            .unwrap();
        o.execute_automation_run(&run.id).unwrap();
        let completed = wait_automation(&o, &run.id, RunPhase::Completed).await;
        assert_eq!(completed.output.text, "PARENT_FINISHED");
        let tasks: Vec<TaskRecord> = ark
            .engine()
            .store
            .list_records(TASK, Some(project.id.as_str()))
            .unwrap()
            .into_iter()
            .map(|value| serde_json::from_value(value).unwrap())
            .collect();
        let delegated = tasks.iter().find(|task| task.owner == "child").unwrap();
        assert_eq!(delegated.state, TaskState::Done);
        assert_eq!(delegated.run.as_deref(), Some(run.id.as_str()));
        assert_eq!(delegated.item.as_deref(), Some(run.work.as_str()));
        assert_eq!(delegated.execution_step.as_deref(), Some("review"));
        assert_ne!(
            delegated.execution_scope.as_deref(),
            Some(child_agent.scope.as_str())
        );
        assert_ne!(
            delegated.conversation.as_deref(),
            Some(run.steps[0].conversation.as_str())
        );
        assert_eq!(parent.calls.lock().unwrap().len(), 2);
        assert_eq!(child.calls.lock().unwrap().len(), 1);
        let input = format!("{:?}", child.calls.lock().unwrap()[0].messages);
        assert!(input.contains("Only this delegated brief"));
        assert!(!input.contains("Review the supplied text"));
        assert_eq!(
            o.work_page("person", false, None, 50)
                .unwrap()
                .entries
                .len(),
            1
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn automation_descendants_cannot_reset_quota_or_delegate_to_an_ancestor() {
    use crate::automation::*;
    use ark_model::{Completion, ToolCall};
    for cycle in [false, true] {
        let reply = |text: &str| Completion {
            content: text.into(),
            tool_calls: vec![],
            usage: Default::default(),
        };
        let delegate = |to: &str| Completion {
            content: String::new(),
            tool_calls: vec![ToolCall {
                id: format!("to-{to}"),
                name: tools::DELEGATE.into(),
                arguments: serde_json::json!({"to":to,"objective":"Bounded child brief","wait":true}),
            }],
            usage: Default::default(),
        };
        let dir = tempfile::tempdir().unwrap();
        let parent = Arc::new(ark_model::MockProvider::new(
            "parent",
            vec![delegate("child"), reply("Parent retained refusal")],
        ));
        let child = Arc::new(ark_model::MockProvider::new(
            "child",
            vec![
                delegate(if cycle { "parent" } else { "leaf" }),
                reply("Child retained refusal"),
            ],
        ));
        let leaf = Arc::new(ark_model::MockProvider::new(
            "leaf",
            vec![reply("MUST_NOT_RUN")],
        ));
        let ark = Ark::builder()
            .store_root(dir.path())
            .provider(parent.clone())
            .planner(false)
            .critic(false)
            .build()
            .unwrap();
        let project = ark.create_project("transitive limits", "").unwrap();
        let o = Orchestrator::builder(&ark, &project)
            .max_parallel(1)
            .models(Arc::new(
                crate::Models::builder()
                    .default_model("parent")
                    .provider("parent", parent.clone())
                    .provider("child-model", child.clone())
                    .provider("leaf-model", leaf.clone())
                    .build(),
            ))
            .build()
            .unwrap();
        o.spawn(
            AgentSpec::new("parent")
                .model("parent")
                .delegates_to(["child"]),
        )
        .await
        .unwrap();
        o.spawn(
            AgentSpec::new("child")
                .model("child-model")
                .delegates_to([if cycle { "parent" } else { "leaf" }]),
        )
        .await
        .unwrap();
        o.spawn(AgentSpec::new("leaf").model("leaf-model"))
            .await
            .unwrap();
        let mut draft = sequence_draft();
        if let StepAction::Agent { target, .. } = &mut draft.steps[0].action {
            *target = WorkTarget::Agent {
                name: "parent".into(),
            };
        }
        draft.authority.actions = vec![AllowedAction {
            action: tools::DELEGATE.into(),
            inputs: std::collections::BTreeMap::from([
                ("to".into(), Bound::Free),
                ("objective".into(), Bound::Free),
                (
                    "wait".into(),
                    Bound::Literal {
                        value: serde_json::json!(true),
                    },
                ),
            ]),
            unattended: false,
            per_run: if cycle { 8 } else { 1 },
            quota: Quota {
                count: 30,
                window_ms: 3_600_000,
            },
        }];
        let saved = o.save_automation("create", None, None, draft).unwrap();
        let run = o
            .admit_confirmed_test_run(
                &saved.automation,
                1,
                RunOrigin::Manual {
                    request: "one".into(),
                },
                Default::default(),
            )
            .await
            .unwrap();
        o.execute_automation_run(&run.id).unwrap();
        let final_run = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let value = o.automation_run(&run.id).unwrap();
                if value.phase.is_final() {
                    break value;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        if !cycle {
            assert_eq!(final_run.phase, RunPhase::Failed);
        }
        assert!(leaf.calls.lock().unwrap().is_empty());
        let tasks: Vec<TaskRecord> = ark
            .engine()
            .store
            .list_records(TASK, Some(project.id.as_str()))
            .unwrap()
            .into_iter()
            .map(|value| serde_json::from_value(value).unwrap())
            .collect();
        assert_eq!(tasks.len(), 2);
        assert_eq!(
            tasks.iter().filter(|task| task.owner == "parent").count(),
            1
        );
        assert_eq!(tasks.iter().filter(|task| task.owner == "child").count(), 1);
        let child_task = tasks.iter().find(|task| task.owner == "child").unwrap();
        let calls = ark
            .engine()
            .work_calls(
                &ProjectId(child_task.execution_scope.clone().unwrap()),
                &child_task.id,
            )
            .unwrap();
        if cycle {
            assert!(calls.iter().any(|call| call
                .error
                .as_deref()
                .is_some_and(|error| error.contains("ancestor"))));
        } else {
            assert!(
                final_run
                    .error
                    .as_deref()
                    .is_some_and(|error| error.contains("limit")),
                "{:?}",
                final_run.error
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn parallel_descendants_can_open_separate_approvals_while_run_is_waiting() {
    use crate::automation::*;
    use ark_model::{Completion, ToolCall};
    use ark_tools::{ActionOutput, Adapter, RunCtx};
    use serde_json::json;
    struct Writes(Arc<std::sync::atomic::AtomicUsize>);
    #[async_trait::async_trait]
    impl Adapter for Writes {
        fn name(&self) -> &str {
            "write"
        }
        fn actions(&self) -> Vec<ark_core::ActionDescriptor> {
            vec![ark_core::ActionDescriptor::new("write.once", "Write the reviewed value", ark_core::RiskTier::Mutate).with_input_schema(json!({"type":"object","required":["value"],"properties":{"value":{"type":"string"}}}))]
        }
        async fn run(
            &self,
            _: &str,
            _: serde_json::Value,
            _: &RunCtx<'_>,
        ) -> ark_tools::Result<ActionOutput> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(ActionOutput::json(json!({"written":true})))
        }
    }
    struct Delayed {
        provider: Arc<ark_model::MockProvider>,
        release: Arc<tokio::sync::Notify>,
    }
    #[async_trait::async_trait]
    impl ark_model::Provider for Delayed {
        fn host_controls_tools(&self) -> bool {
            true
        }
        fn describe(&self) -> String {
            "delayed synthetic provider".into()
        }
        async fn complete(
            &self,
            system: &str,
            messages: &[ark_model::Message],
            tools: &[ark_model::ToolDef],
        ) -> ark_model::Result<Completion> {
            self.release.notified().await;
            self.provider.complete(system, messages, tools).await
        }
        async fn complete_streaming(
            &self,
            system: &str,
            messages: &[ark_model::Message],
            tools: &[ark_model::ToolDef],
            sink: ark_model::TextSink<'_>,
        ) -> ark_model::Result<Completion> {
            if self.provider.calls.lock().unwrap().is_empty() {
                self.release.notified().await;
            }
            self.provider
                .complete_streaming(system, messages, tools, sink)
                .await
        }
    }
    for cancel in [false, true] {
        let reply = |text: &str| Completion {
            content: text.into(),
            tool_calls: vec![],
            usage: Default::default(),
        };
        let call = |name: &str, arguments| ToolCall {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.into(),
            arguments,
        };
        let dir = tempfile::tempdir().unwrap();
        let writes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let parent = Arc::new(ark_model::MockProvider::new(
            "parent",
            vec![
                Completion {
                    content: String::new(),
                    tool_calls: vec![
                        call(
                            tools::DELEGATE,
                            json!({"to":"first","objective":"First independent brief","wait":true}),
                        ),
                        call(
                            tools::DELEGATE,
                            json!({"to":"second","objective":"Second independent brief","wait":true}),
                        ),
                    ],
                    usage: Default::default(),
                },
                reply("Parent result"),
            ],
        ));
        let first = Arc::new(ark_model::MockProvider::new(
            "first",
            vec![
                Completion {
                    content: String::new(),
                    tool_calls: vec![call("write.once", json!({"value":"first"}))],
                    usage: Default::default(),
                },
                reply("First result"),
            ],
        ));
        let second = Arc::new(ark_model::MockProvider::new(
            "second",
            vec![
                Completion {
                    content: String::new(),
                    tool_calls: vec![call("write.once", json!({"value":"second"}))],
                    usage: Default::default(),
                },
                reply("Second result"),
            ],
        ));
        let release = Arc::new(tokio::sync::Notify::new());
        let profile = AgentProfile {
            core_actions: vec![tools::DELEGATE.into(), "write.once".into()],
            ..Default::default()
        };
        let ark = Ark::builder()
            .store_root(dir.path())
            .provider(parent.clone())
            .tool(Arc::new(Writes(writes.clone())))
            .profile(profile)
            .planner(false)
            .critic(false)
            .build()
            .unwrap();
        let project = ark.create_project("parallel approvals", "").unwrap();
        let models = crate::Models::builder()
            .default_model("parent")
            .provider("parent", parent)
            .provider("first-model", first)
            .provider(
                "second-model",
                Arc::new(Delayed {
                    provider: second,
                    release: release.clone(),
                }),
            )
            .build();
        let o = Orchestrator::builder(&ark, &project)
            .max_parallel(3)
            .models(Arc::new(models))
            .build()
            .unwrap();
        o.spawn(AgentSpec::new("first").model("first-model"))
            .await
            .unwrap();
        o.spawn(AgentSpec::new("second").model("second-model"))
            .await
            .unwrap();
        let mut draft = sequence_draft();
        if let StepAction::Agent { target, .. } = &mut draft.steps[0].action {
            *target = WorkTarget::General {
                model: Some("parent".into()),
            };
        }
        draft.authority.actions = vec![
            AllowedAction {
                action: tools::DELEGATE.into(),
                inputs: std::collections::BTreeMap::from([
                    ("to".into(), Bound::Free),
                    ("objective".into(), Bound::Free),
                    ("wait".into(), Bound::Literal { value: json!(true) }),
                ]),
                unattended: false,
                per_run: 2,
                quota: Quota {
                    count: 30,
                    window_ms: 3_600_000,
                },
            },
            AllowedAction {
                action: "write.once".into(),
                inputs: std::collections::BTreeMap::from([("value".into(), Bound::Free)]),
                unattended: false,
                per_run: 2,
                quota: Quota {
                    count: 30,
                    window_ms: 3_600_000,
                },
            },
        ];
        let saved = o.save_automation("create", None, None, draft).unwrap();
        let run = o
            .admit_confirmed_test_run(
                &saved.automation,
                1,
                RunOrigin::Manual {
                    request: "one".into(),
                },
                Default::default(),
            )
            .await
            .unwrap();
        o.execute_automation_run(&run.id).unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let value = o.automation_run(&run.id).unwrap();
                if value.questions.len() == 1 {
                    break;
                }
                assert!(!value.phase.is_final(), "{:?}", value.error);
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        release.notify_one();
        let waiting = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let value = o.automation_run(&run.id).unwrap();
                if value.questions.len() == 2 {
                    break value;
                }
                assert!(!value.phase.is_final(), "{:?}", value.error);
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(waiting.phase, RunPhase::WaitingApproval);
        assert_eq!(writes.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(waiting.questions.values().all(|question| question
            .action
            .as_ref()
            .is_some_and(|action| action.task.is_some() && action.agent.is_some())));
        if cancel {
            o.cancel_automation_run(&run.id).unwrap();
        } else {
            let second = waiting
                .questions
                .values()
                .find(|question| {
                    question.action.as_ref().unwrap().agent.as_deref() == Some("second")
                })
                .unwrap();
            o.answer_automation_question(
                &run.id,
                &second.id,
                &second.digest,
                ApprovalDecision::Deny,
            )
            .unwrap();
            let first = waiting
                .questions
                .values()
                .find(|question| {
                    question.action.as_ref().unwrap().agent.as_deref() == Some("first")
                })
                .unwrap();
            o.answer_automation_question(
                &run.id,
                &first.id,
                &first.digest,
                ApprovalDecision::AllowOnce,
            )
            .unwrap();
        }
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if o.automation_run(&run.id).unwrap().phase.is_final() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            writes.load(std::sync::atomic::Ordering::SeqCst),
            usize::from(!cancel)
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn work_contexts_isolate_state_and_explicit_files_and_survive_reopen_and_compaction() {
    use ark_core::{ArtifactKind, Claim, ClaimType, Provenance, ResearchState};
    for agent_work in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let reply = |text: &str| ark_model::Completion {
            content: text.into(),
            tool_calls: vec![],
            usage: Default::default(),
        };
        let provider = Arc::new(ark_model::MockProvider::new(
            "test",
            vec![
                reply("ALPHA_ANSWER"),
                reply("BETA_ANSWER"),
                reply("ALPHA_FOLLOWUP"),
            ],
        ));
        let ark = Ark::builder()
            .store_root(dir.path())
            .provider(provider.clone())
            .planner(false)
            .critic(false)
            .build()
            .unwrap();
        let project = ark.create_project("isolation", "").unwrap();
        let models = || -> Arc<dyn ModelResolver> {
            Arc::new(
                crate::Models::builder()
                    .default_model("test")
                    .provider("test", provider.clone())
                    .build(),
            )
        };
        let o = Orchestrator::builder(&ark, &project)
            .models(models())
            .build()
            .unwrap();
        let target = if agent_work {
            o.spawn(AgentSpec::new("reader")).await.unwrap();
            WorkTarget::Agent {
                name: "reader".into(),
            }
        } else {
            WorkTarget::General {
                model: Some("test".into()),
            }
        };
        let seeded = |text: &str| {
            let mut state = ResearchState::new("other objective");
            state.findings.push(Claim::new(text, ClaimType::Fact));
            state
        };
        ark.engine()
            .store
            .put_research_state(&project.id, "", &seeded("ROOT_PRIVATE_FINDING"))
            .unwrap();
        if agent_work {
            ark.engine()
                .store
                .put_research_state(
                    &ProjectId(o.agent("reader").unwrap().scope),
                    "",
                    &seeded("PRIMARY_AGENT_FINDING"),
                )
                .unwrap();
        }
        let file = ark
            .engine()
            .ingest_bytes(
                &project.id,
                "ALPHA_EXPLICIT_INPUT.txt",
                ArtifactKind::Text,
                b"Alpha explicit file",
                Provenance::Ingested {
                    path: "alpha.txt".into(),
                },
            )
            .unwrap();
        let alpha_text = format!("Inspect the supplied art:{}", file.id);
        let alpha = o
            .admit_work(
                target.clone(),
                TaskSpec::new(&alpha_text),
                "desktop",
                "alpha",
            )
            .await
            .unwrap();
        let alpha_session = Session::open(ark.engine(), &alpha.conversation)
            .unwrap()
            .unwrap();
        let alpha_scope = alpha_session.execution_project(ark.engine()).unwrap();
        assert_ne!(alpha_scope, project.id);
        ark.engine()
            .store
            .put_research_state(
                &alpha_scope,
                alpha_session.id().as_str(),
                &seeded("ALPHA_PRIVATE_FINDING"),
            )
            .unwrap();
        if agent_work {
            o.execute_work(&alpha.id).unwrap();
            o.wait(alpha.task.as_deref().unwrap()).await.unwrap();
        } else {
            let mut session = alpha_session;
            ark.ask_session(&mut session, &alpha_text, Default::default(), |_| {})
                .await
                .unwrap();
        }
        let beta = o
            .admit_work(
                target.clone(),
                TaskSpec::new("Independent beta objective"),
                "desktop",
                "beta",
            )
            .await
            .unwrap();
        let beta_scope = Session::open(ark.engine(), &beta.conversation)
            .unwrap()
            .unwrap()
            .execution_project(ark.engine())
            .unwrap();
        assert_ne!(alpha_scope, beta_scope);
        if agent_work {
            o.execute_work(&beta.id).unwrap();
            o.wait(beta.task.as_deref().unwrap()).await.unwrap();
        } else {
            let mut session = Session::open(ark.engine(), &beta.conversation)
                .unwrap()
                .unwrap();
            ark.ask_session(
                &mut session,
                "Independent beta objective",
                Default::default(),
                |_| {},
            )
            .await
            .unwrap();
        }
        assert!(ark
            .engine()
            .store
            .get_artifact(&alpha_scope, &file.id)
            .unwrap()
            .is_some());
        assert!(ark
            .engine()
            .store
            .get_artifact(&beta_scope, &file.id)
            .unwrap()
            .is_none());
        let o = Orchestrator::builder(&ark, &project)
            .models(models())
            .build()
            .unwrap();
        let mut reopened = Session::open(ark.engine(), &alpha.conversation)
            .unwrap()
            .unwrap();
        assert_eq!(
            reopened.execution_project(ark.engine()).unwrap(),
            alpha_scope
        );
        reopened.compact(ark.engine()).unwrap();
        let follow = o
            .admit_work(
                target,
                TaskSpec::new("Continue alpha").item(&alpha.work),
                "desktop",
                "follow",
            )
            .await
            .unwrap();
        assert_eq!(follow.conversation, alpha.conversation);
        if agent_work {
            o.execute_work(&follow.id).unwrap();
            o.wait(follow.task.as_deref().unwrap()).await.unwrap();
        } else {
            ark.ask_session(&mut reopened, "Continue alpha", Default::default(), |_| {})
                .await
                .unwrap();
        }
        let calls = provider.recorded();
        assert_eq!(calls.len(), 3);
        let request = |index: usize| format!("{} {:?}", calls[index].system, calls[index].messages);
        assert!(request(0).contains("ALPHA_PRIVATE_FINDING"));
        assert!(request(0).contains("ALPHA_EXPLICIT_INPUT.txt"));
        assert!(!request(1).contains("ALPHA_PRIVATE_FINDING"));
        assert!(!request(1).contains("ALPHA_EXPLICIT_INPUT.txt"));
        assert!(request(2).contains("ALPHA_PRIVATE_FINDING"));
        for index in 0..3 {
            assert!(!request(index).contains("ROOT_PRIVATE_FINDING"));
            assert!(!request(index).contains("PRIMARY_AGENT_FINDING"));
        }
        let mut forged = Session::open(ark.engine(), &beta.conversation)
            .unwrap()
            .unwrap()
            .record()
            .clone();
        forged.execution_scope = Some(alpha_scope);
        ark.engine()
            .store
            .put_session(
                &beta.conversation,
                &project.id,
                &serde_json::to_value(forged).unwrap(),
            )
            .unwrap();
        let mut forged = Session::open(ark.engine(), &beta.conversation)
            .unwrap()
            .unwrap();
        assert!(ark
            .ask_session(&mut forged, "must refuse", Default::default(), |_| {})
            .await
            .is_err());
        assert_eq!(provider.recorded().len(), 3);
    }
}

#[tokio::test]
async fn automation_recovers_exact_pending_approval_before_any_new_model_decision() {
    use crate::automation::*;
    use ark_core::{ActionDescriptor, InvocationStatus, RiskTier};
    use ark_tools::{ActionOutput, Adapter, RunCtx};
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Write(Arc<AtomicUsize>);
    #[async_trait::async_trait]
    impl Adapter for Write {
        fn name(&self) -> &str {
            "review"
        }
        fn actions(&self) -> Vec<ActionDescriptor> {
            vec![ActionDescriptor::new(
                "review.write",
                "Write the reviewed value",
                RiskTier::Mutate,
            )]
        }
        async fn run(
            &self,
            _: &str,
            input: serde_json::Value,
            _: &RunCtx<'_>,
        ) -> ark_tools::Result<ActionOutput> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(ActionOutput::json(input))
        }
    }
    for retained_question in [false, true] {
        for allow in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let writes = Arc::new(AtomicUsize::new(0));
            let provider = Arc::new(ark_model::MockProvider::new(
                "test",
                vec![ark_model::Completion {
                    content: "RECOVERED_REVIEWED_RESULT".into(),
                    tool_calls: vec![],
                    usage: Default::default(),
                }],
            ));
            let ark = Ark::builder()
                .store_root(dir.path())
                .provider(provider.clone())
                .tool(Arc::new(Write(writes.clone())))
                .planner(false)
                .critic(false)
                .build()
                .unwrap();
            let project = ark.create_project("pending recovery", "").unwrap();
            let o = Orchestrator::builder(&ark, &project)
                .models(Arc::new(
                    crate::Models::builder()
                        .default_model("test")
                        .provider("test", provider.clone())
                        .build(),
                ))
                .build()
                .unwrap();
            let mut draft = sequence_draft();
            draft.authority.actions = vec![AllowedAction {
                action: "review.write".into(),
                inputs: std::collections::BTreeMap::from([("value".into(), Bound::Free)]),
                unattended: false,
                per_run: 4,
                quota: Quota {
                    count: 4,
                    window_ms: 3_600_000,
                },
            }];
            let saved = o.save_automation("new", None, None, draft).unwrap();
            let mut run = o
                .admit_confirmed_test_run(
                    &saved.automation,
                    1,
                    RunOrigin::Manual {
                        request: "restart".into(),
                    },
                    Default::default(),
                )
                .await
                .unwrap();
            let work = format!("{}/{}", run.id, run.steps[0].id);
            let scope = ProjectId(run.steps[0].scope.clone());
            let pending = ark
                .engine()
                .for_work(&work)
                .run_action(
                    &scope,
                    "review.write",
                    json!({"value":"frozen-before-crash"}),
                    false,
                )
                .await
                .unwrap();
            assert_eq!(pending.status, InvocationStatus::PendingApproval);
            run.phase = RunPhase::WaitingApproval;
            run.steps[0].phase = StepPhase::Running;
            run.owner = Some(RunOwner {
                token: "dead-owner".into(),
                pid: 999_999,
                pid_start: None,
            });
            if retained_question {
                let request = ApprovalRequest {
                    invocation: Some(pending.id.clone()),
                    project: Some(scope.clone()),
                    action: pending.action.clone(),
                    summary: "Write the reviewed value".into(),
                    risk: pending.risk,
                    input: pending.input.clone(),
                    class: pending.action.clone(),
                    preview: ark_runtime::approval::preview_call(&pending.action, &pending.input),
                    agent: None,
                    task: None,
                    ask_each_time: true,
                    cut_short: false,
                };
                let digest = format!(
                    "v1:{}",
                    blake3::hash(
                        &serde_json::to_vec(&(&run.revision.digest, &run.steps[0].id, &request))
                            .unwrap()
                    )
                    .to_hex()
                );
                run.questions.insert(
                    "retained-question".into(),
                    RunQuestion {
                        id: "retained-question".into(),
                        title: request.preview.clone(),
                        detail: request.summary.clone(),
                        digest,
                        action: Some(request),
                        files: vec![],
                        decision: None,
                    },
                );
            }
            ark.engine()
                .store
                .put_record(
                    RUN,
                    &run.id,
                    project.id.as_str(),
                    &serde_json::to_value(&run).unwrap(),
                )
                .unwrap();
            o.execute_automation_run(&run.id).unwrap();
            let waiting = wait_automation(&o, &run.id, RunPhase::WaitingApproval).await;
            let waiting = if waiting.questions.is_empty() {
                tokio::time::timeout(std::time::Duration::from_secs(2), async {
                    loop {
                        let current = o.automation_run(&run.id).unwrap();
                        if !current.questions.is_empty() {
                            break current;
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    }
                })
                .await
                .unwrap()
            } else {
                waiting
            };
            assert_eq!(waiting.questions.len(), 1);
            assert!(provider.calls.lock().unwrap().is_empty());
            assert_eq!(writes.load(Ordering::SeqCst), 0);
            let question = waiting.questions.values().next().unwrap();
            assert_eq!(
                question.action.as_ref().unwrap().invocation.as_ref(),
                Some(&pending.id)
            );
            if retained_question {
                assert_eq!(question.id, "retained-question");
            }
            let decision = if allow {
                ApprovalDecision::AllowOnce
            } else {
                ApprovalDecision::Deny
            };
            o.answer_automation_question(&run.id, &question.id, &question.digest, decision)
                .unwrap();
            let finished = wait_automation(
                &o,
                &run.id,
                if allow {
                    RunPhase::Completed
                } else {
                    RunPhase::Failed
                },
            )
            .await;
            assert_eq!(finished.questions.len(), 1);
            assert_eq!(writes.load(Ordering::SeqCst), usize::from(allow));
            assert_eq!(
                ark.engine()
                    .store
                    .get_invocation(&scope, pending.id.as_str())
                    .unwrap()
                    .unwrap()
                    .status,
                if allow {
                    InvocationStatus::Ok
                } else {
                    InvocationStatus::Denied
                }
            );
            assert_eq!(provider.calls.lock().unwrap().len(), usize::from(allow));
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pending_automation_approval_survives_process_exit_for_general_and_agent() {
    use crate::automation::*;
    use ark_tools::{ActionOutput, Adapter, RunCtx};
    use serde_json::json;
    struct Write(std::path::PathBuf);
    #[async_trait::async_trait]
    impl Adapter for Write {
        fn name(&self) -> &str {
            "restart"
        }
        fn actions(&self) -> Vec<ark_core::ActionDescriptor> {
            vec![ark_core::ActionDescriptor::new(
                "restart.write",
                "Write exactly once after review",
                ark_core::RiskTier::Mutate,
            )]
        }
        async fn run(
            &self,
            _: &str,
            input: serde_json::Value,
            _: &RunCtx<'_>,
        ) -> ark_tools::Result<ActionOutput> {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.0)?;
            writeln!(file, "{}", input)?;
            Ok(ActionOutput::json(input))
        }
    }
    async fn open(
        root: &std::path::Path,
        child: bool,
        native: bool,
    ) -> (Ark, Project, Orchestrator, Arc<ark_model::MockProvider>) {
        let reply = if child {
            ark_model::Completion {
                content: String::new(),
                tool_calls: vec![ark_model::ToolCall {
                    id: "reviewed-call".into(),
                    name: "restart.write".into(),
                    arguments: json!({"value":"exact-before-process-exit"}),
                }],
                usage: Default::default(),
            }
        } else {
            ark_model::Completion {
                content: "CONFIRMED_AFTER_RESTART".into(),
                tool_calls: vec![],
                usage: Default::default(),
            }
        };
        let provider = Arc::new(ark_model::MockProvider::new("test", vec![reply]));
        let ark = Ark::builder()
            .store_root(root)
            .provider(provider.clone())
            .tool(Arc::new(Write(root.join("effects.log"))))
            .profile(AgentProfile {
                core_actions: vec!["restart.write".into()],
                approval_classifier: Some(Arc::new(|_, _| "custom-review-class".into())),
                ..Default::default()
            })
            .planner(false)
            .critic(false)
            .build()
            .unwrap();
        let project = if child {
            ark.create_project("approval process boundary", "").unwrap()
        } else {
            serde_json::from_slice(&std::fs::read(root.join("project.json")).unwrap()).unwrap()
        };
        let o = Orchestrator::builder(&ark, &project)
            .models(Arc::new(
                crate::Models::builder()
                    .default_model("test")
                    .provider("test", provider.clone())
                    .build(),
            ))
            .build()
            .unwrap();
        if child && native {
            o.spawn(
                AgentSpec::new("reviewer")
                    .model("test")
                    .tools(["restart.write"]),
            )
            .await
            .unwrap();
        }
        (ark, project, o, provider)
    }
    if let Some(root) = std::env::var_os("ARK_TEST_PENDING_APPROVAL_PROCESS") {
        let root = std::path::PathBuf::from(root);
        let native = std::env::var("ARK_TEST_PENDING_APPROVAL_NATIVE").unwrap() == "true";
        let (_ark, project, o, _) = open(&root, true, native).await;
        let mut draft = sequence_draft();
        if native {
            if let StepAction::Agent { target, .. } = &mut draft.steps[0].action {
                *target = WorkTarget::Agent {
                    name: "reviewer".into(),
                };
            }
        }
        draft.authority.actions = vec![AllowedAction {
            action: "restart.write".into(),
            inputs: std::collections::BTreeMap::from([("value".into(), Bound::Free)]),
            unattended: false,
            per_run: 2,
            quota: Quota {
                count: 2,
                window_ms: 3_600_000,
            },
        }];
        let saved = o.save_automation("new", None, None, draft).unwrap();
        let run = o
            .admit_confirmed_test_run(
                &saved.automation,
                1,
                RunOrigin::Manual {
                    request: "process-exit".into(),
                },
                Default::default(),
            )
            .await
            .unwrap();
        std::fs::write(
            root.join("project.json"),
            serde_json::to_vec(&project).unwrap(),
        )
        .unwrap();
        std::fs::write(root.join("run-id"), &run.id).unwrap();
        if std::env::var("ARK_TEST_PENDING_APPROVAL_AFTER_RESULT").as_deref() == Ok("true") {
            struct ExitAfterResult;
            impl ark_runtime::guard::CallGuard for ExitAfterResult {
                fn check(
                    &self,
                    _: &str,
                    _: &serde_json::Value,
                    _: Option<&ark_core::ActionDescriptor>,
                ) -> ark_runtime::guard::Verdict {
                    ark_runtime::guard::Verdict::Allow
                }
                fn prepare(&self, _: &ProjectId, _: &ark_core::Invocation) -> Result<(), String> {
                    Ok(())
                }
                fn started(&self, _: &ProjectId, _: &ark_core::Invocation) -> Result<(), String> {
                    Ok(())
                }
                fn ended(&self, _: &ProjectId, _: &ark_core::Invocation) -> Result<(), String> {
                    std::process::exit(0)
                }
                fn aborted(
                    &self,
                    _: &ProjectId,
                    _: &ark_core::Invocation,
                    _: bool,
                ) -> Result<(), String> {
                    Ok(())
                }
            }
            let mut run = run;
            run.phase = RunPhase::Running;
            run.steps[0].phase = StepPhase::Running;
            run.owner = Some(RunOwner {
                token: o.inner.execution_token.clone(),
                pid: std::process::id(),
                pid_start: process::own_start(),
            });
            _ark.engine()
                .store
                .put_record(
                    RUN,
                    &run.id,
                    project.id.as_str(),
                    &serde_json::to_value(&run).unwrap(),
                )
                .unwrap();
            let work = format!("{}/{}", run.id, run.steps[0].id);
            let scope = ProjectId(run.steps[0].scope.clone());
            let pending = _ark
                .engine()
                .for_work(&work)
                .run_action(
                    &scope,
                    "restart.write",
                    json!({"value":"exact-before-process-exit"}),
                    false,
                )
                .await
                .unwrap();
            let engine = _ark
                .engine()
                .for_work(&work)
                .guarded(Arc::new(ExitAfterResult))
                .guarded(o.inner.run_guard(&run.id, true).unwrap());
            let gate = ApprovalGate::new(Arc::new(ark_runtime::FixedApprover(
                ApprovalDecision::AllowOnce,
            )));
            engine
                .run_pending_with_approval(&scope, &pending.id, &gate)
                .await
                .unwrap();
            panic!("the child must exit after retaining its action result");
        }
        o.execute_automation_run(&run.id).unwrap();
        let waiting = wait_automation(&o, &run.id, RunPhase::WaitingApproval).await;
        assert_eq!(waiting.questions.len(), 1);
        assert!(!root.join("effects.log").exists());
        std::process::exit(0);
    }
    for (native, after_result) in [(false, false), (true, false), (false, true)] {
        let dir = tempfile::tempdir().unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact", "orchestrator::conversation_tests::pending_automation_approval_survives_process_exit_for_general_and_agent", "--nocapture"]).env("ARK_TEST_PENDING_APPROVAL_PROCESS", dir.path()).env("ARK_TEST_PENDING_APPROVAL_NATIVE", native.to_string()).env("ARK_TEST_PENDING_APPROVAL_AFTER_RESULT", after_result.to_string()).status().unwrap();
        assert!(status.success());
        let (_ark, _project, o, provider) = open(dir.path(), false, native).await;
        let run_id = std::fs::read_to_string(dir.path().join("run-id")).unwrap();
        if after_result {
            let completed = wait_automation(&o, &run_id, RunPhase::Completed).await;
            assert_eq!(completed.output.text, "CONFIRMED_AFTER_RESTART");
            assert_eq!(
                std::fs::read_to_string(dir.path().join("effects.log"))
                    .unwrap()
                    .lines()
                    .count(),
                1
            );
            assert_eq!(provider.calls.lock().unwrap().len(), 1);
            continue;
        }
        let waiting = wait_automation(&o, &run_id, RunPhase::WaitingApproval).await;
        assert_eq!(waiting.questions.len(), 1);
        let question = waiting.questions.values().next().unwrap();
        assert!(question.action.as_ref().unwrap().invocation.is_some());
        assert_eq!(
            question.action.as_ref().unwrap().class,
            "custom-review-class"
        );
        assert!(provider.calls.lock().unwrap().is_empty());
        assert!(!dir.path().join("effects.log").exists());
        o.answer_automation_question(
            &run_id,
            &question.id,
            &question.digest,
            ApprovalDecision::AllowOnce,
        )
        .unwrap();
        let completed = wait_automation(&o, &run_id, RunPhase::Completed).await;
        assert_eq!(completed.questions.len(), 1);
        let effects = std::fs::read_to_string(dir.path().join("effects.log")).unwrap();
        assert_eq!(effects.lines().count(), 1);
        assert!(effects.contains("exact-before-process-exit"));
        assert_eq!(provider.calls.lock().unwrap().len(), 1);
        assert_eq!(completed.output.text, "CONFIRMED_AFTER_RESTART");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn legacy_schedule_runs_keep_one_item_and_independent_conversations_with_exact_followups() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ark_model::MockProvider::new(
        "test",
        [
            "First scheduled result",
            "Second scheduled result",
            "Follow up on first",
        ]
        .into_iter()
        .map(|answer| ark_model::Completion {
            content: answer.into(),
            tool_calls: vec![],
            usage: Default::default(),
        })
        .collect(),
    ));
    let ark = Ark::builder()
        .store_root(dir.path())
        .provider(provider.clone())
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.create_project("legacy scheduled work", "").unwrap();
    let o = Orchestrator::builder(&ark, &project)
        .models(Arc::new(
            crate::Models::builder()
                .default_model("test")
                .provider("test", provider.clone())
                .build(),
        ))
        .build()
        .unwrap();
    o.spawn(AgentSpec::new("scheduled").model("test"))
        .await
        .unwrap();
    let item = o
        .add_work("person", "Recurring report", Default::default())
        .unwrap();
    let destination = Session::start(ark.engine(), &project.id).unwrap();
    let mut spec = TaskSpec::new("Report")
        .context("Preserve the full context")
        .budget(crate::Budget::default().steps(4).tokens(20_000));
    spec.item = Some(item.id.clone());
    spec.report_to = Some(destination.id().to_string());
    spec.work = Some("original-caller-work".into());
    let schedule = o
        .schedule("scheduled", spec.clone(), When::every_minutes(5))
        .unwrap();
    let first = o
        .run_schedule_now_with_key(&schedule.id, "first")
        .await
        .unwrap();
    assert_eq!(o.wait(&first).await.unwrap().state, TaskState::Done);
    let second = o
        .run_schedule_now_with_key(&schedule.id, "second")
        .await
        .unwrap();
    assert_eq!(o.wait(&second).await.unwrap().state, TaskState::Done);
    assert_eq!(
        o.run_schedule_now_with_key(&schedule.id, "first")
            .await
            .unwrap(),
        first
    );
    let one = o.task(&first).unwrap();
    let two = o.task(&second).unwrap();
    assert_ne!(one.conversation, two.conversation);
    assert_ne!(one.execution_scope, two.execution_scope);
    assert_eq!(one.item.as_deref(), Some(item.id.as_str()));
    assert_eq!(two.item, one.item);
    assert_eq!(one.report_to, spec.report_to);
    assert_eq!(two.report_to, spec.report_to);
    assert_eq!(one.work, spec.work);
    assert_eq!(one.budget.max_tokens, Some(20_000));
    assert!(one.context.contains(&spec.context));
    assert_eq!(o.board().len(), 1);
    let page = o.work_detail("person", &item.id).unwrap();
    let entry = &page.entries[0];
    assert_eq!(entry.receipts.len(), 2);
    let receipt = entry
        .receipts
        .iter()
        .find(|receipt| receipt.task.as_deref() == Some(&first))
        .unwrap();
    o.spawn(AgentSpec::new("other").model("test"))
        .await
        .unwrap();
    let occupied = o
        .admit_work(
            WorkTarget::Agent {
                name: "other".into(),
            },
            TaskSpec::new("Other agent owns the next turn").item(item.id.clone()),
            "manual",
            "other-agent-hold",
        )
        .await
        .unwrap();
    let blocked = o
        .try_admit_work_followup(
            WorkTarget::Agent {
                name: "scheduled".into(),
            },
            TaskSpec::new("Continue the first execution").item(item.id.clone()),
            "manual",
            "followup-first",
            &receipt.id,
        )
        .await
        .unwrap();
    assert!(blocked.is_none());
    assert!(o
        .work_receipt("manual", "followup-first")
        .unwrap()
        .is_none());
    o.execute_work(&occupied.id).unwrap();
    o.cancel(occupied.task.as_ref().unwrap()).unwrap();
    assert_eq!(
        o.wait(occupied.task.as_ref().unwrap()).await.unwrap().state,
        TaskState::Cancelled
    );
    let before_followup_turns = Session::open(ark.engine(), one.conversation.as_ref().unwrap())
        .unwrap()
        .unwrap()
        .turn_count();
    assert_eq!(provider.calls.lock().unwrap().len(), 2);
    let mut followup = TaskSpec::new("Continue the first execution");
    followup.item = Some(item.id.clone());
    let followup = o
        .admit_work_followup(
            WorkTarget::Agent {
                name: "scheduled".into(),
            },
            followup,
            "manual",
            "followup-first",
            &receipt.id,
        )
        .await
        .unwrap();
    assert_eq!(followup.conversation, one.conversation.clone().unwrap());
    o.execute_work(&followup.id).unwrap();
    let continued = o.wait(followup.task.as_ref().unwrap()).await.unwrap();
    assert_eq!(continued.state, TaskState::Done, "{:?}", continued.error);
    assert_eq!(continued.execution_scope, one.execution_scope);
    assert_eq!(provider.calls.lock().unwrap().len(), 3);
    assert_eq!(
        Session::open(ark.engine(), one.conversation.as_ref().unwrap())
            .unwrap()
            .unwrap()
            .turn_count(),
        before_followup_turns + 1
    );
    assert_eq!(
        Session::open(ark.engine(), two.conversation.as_ref().unwrap())
            .unwrap()
            .unwrap()
            .turn_count(),
        1
    );
    let posts =
        ark_runtime::session::posts(ark.engine(), destination.id().as_str(), 0, 100).unwrap();
    assert_eq!(posts.len(), 2);
    let other = o
        .add_work("person", "Other report", Default::default())
        .unwrap();
    let mut wrong = TaskSpec::new("Cross context");
    wrong.item = Some(other.id);
    let before = o.tasks().len();
    assert!(o
        .admit_work_followup(
            WorkTarget::Agent {
                name: "scheduled".into()
            },
            wrong,
            "manual",
            "wrong-scope",
            &receipt.id
        )
        .await
        .is_err());
    assert_eq!(o.tasks().len(), before);
    assert_eq!(
        o.schedule_record(&schedule.id).unwrap().next_ms,
        schedule.next_ms
    );
}

#[tokio::test]
async fn legacy_schedule_preflight_failure_retains_due_occurrence_and_durable_backoff() {
    struct Offline(Arc<std::sync::atomic::AtomicUsize>);
    #[async_trait::async_trait]
    impl ModelResolver for Offline {
        async fn resolve(&self, _: &str, _: Option<&str>) -> Result<crate::AgentModels, OrchError> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(OrchError::NotAllowed(
                "the selected account is offline".into(),
            ))
        }
    }
    let (_dir, ark, project) = fixture();
    let initial = Orchestrator::builder(&ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    initial.spawn(AgentSpec::new("scheduled")).await.unwrap();
    drop(initial);
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let o = Orchestrator::builder(&ark, &project)
        .models(Arc::new(Offline(calls.clone())))
        .build()
        .unwrap();
    let schedule = o
        .schedule("scheduled", TaskSpec::new("Report"), When::every_minutes(5))
        .unwrap();
    let slot = schedule.next_ms.unwrap();
    assert!(o.tick_schedules(slot).await.is_empty());
    let retained = o.schedule_record(&schedule.id).unwrap();
    assert_eq!(retained.next_ms, Some(slot));
    assert_eq!(retained.runs, 0);
    assert_eq!(retained.skipped, 0);
    assert_eq!(retained.admission_failures, 1);
    assert!(retained
        .admission_error
        .as_deref()
        .unwrap()
        .contains("offline"));
    assert!(retained.admission_retry_ms.unwrap() > slot);
    let resolutions = calls.load(std::sync::atomic::Ordering::SeqCst);
    assert!(o.tick_schedules(slot + 1).await.is_empty());
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), resolutions);
    assert!(o.tasks().is_empty());
    assert!(o.board().is_empty());
    assert!(Session::list(ark.engine(), Some(&project.id))
        .unwrap()
        .is_empty());
    let observer = Orchestrator::builder(&ark, &project)
        .models(Arc::new(Offline(calls)))
        .build()
        .unwrap();
    let persisted = observer.schedule_record(&schedule.id).unwrap();
    assert_eq!(persisted.admission_retry_ms, retained.admission_retry_ms);
    assert_eq!(persisted.next_ms, Some(slot));
    assert!(observer
        .tick_schedules(persisted.admission_retry_ms.unwrap())
        .await
        .is_empty());
    let second = observer.schedule_record(&schedule.id).unwrap();
    assert!(observer
        .tick_schedules(second.admission_retry_ms.unwrap())
        .await
        .is_empty());
    let paused = observer.schedule_record(&schedule.id).unwrap();
    assert!(paused.paused);
    assert_eq!(paused.admission_slot_ms, Some(slot));
    assert_eq!(paused.admission_failures, 3);
    let reopened = Orchestrator::builder(&ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    let resumed = reopened.resume_schedule(&schedule.id).unwrap();
    assert!(!resumed.paused);
    assert_eq!(resumed.next_ms, Some(slot));
    assert_eq!(resumed.admission_failures, 0);
    assert!(resumed.admission_error.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_an_uncertain_save_retains_any_committed_revision_and_fences_late_requests() {
    let (_dir, ark, project) = fixture();
    let left = Orchestrator::builder(&ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    let right = Orchestrator::builder(&ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    assert!(left
        .cancel_automation_save("cancel-before-save", None, None, sequence_draft())
        .unwrap()
        .is_none());
    assert!(right
        .save_automation("cancel-before-save", None, None, sequence_draft())
        .is_err());
    assert!(left.automation_page(None, 100).unwrap().entries.is_empty());
    let saved = left
        .save_automation("save-before-cancel", None, None, sequence_draft())
        .unwrap();
    let recovered = right
        .cancel_automation_save("save-before-cancel", None, None, sequence_draft())
        .unwrap()
        .unwrap();
    assert_eq!(recovered.automation, saved.automation);
    assert_eq!(
        right
            .save_automation("save-before-cancel", None, None, sequence_draft())
            .unwrap()
            .revision,
        1
    );
    let mut newer = sequence_draft();
    newer.name = "A newer saved revision".into();
    right
        .save_automation("next-revision", Some(&saved.automation), Some(1), newer)
        .unwrap();
    assert!(left
        .cancel_automation_save(
            "stale-save",
            Some(&saved.automation),
            Some(1),
            sequence_draft()
        )
        .unwrap()
        .is_none());
    assert!(right
        .save_automation(
            "stale-save",
            Some(&saved.automation),
            Some(1),
            sequence_draft()
        )
        .is_err());
    assert_eq!(
        left.automation(&saved.automation).unwrap().revision.number,
        2
    );
    let one = left.clone();
    let two = right.clone();
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let other = barrier.clone();
    let save = std::thread::spawn(move || {
        barrier.wait();
        one.save_automation("racing-save", None, None, sequence_draft())
    });
    let cancel = std::thread::spawn(move || {
        other.wait();
        two.cancel_automation_save("racing-save", None, None, sequence_draft())
    });
    let save = save.join().unwrap();
    let cancel = cancel.join().unwrap().unwrap();
    assert_eq!(save.is_ok(), cancel.is_some());
    let reopened = Orchestrator::builder(&ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    assert_eq!(
        reopened.automation_page(None, 100).unwrap().entries.len(),
        1 + usize::from(save.is_ok())
    );
}

async fn wait_automation_delivery(o: &Orchestrator, run: &str, delivered: bool) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let value = o
                .inner
                .engine
                .store
                .get_record("automation_result_post", run)
                .unwrap()
                .unwrap();
            if if delivered {
                value["state"] == "delivered"
            } else {
                !value["error"].is_null()
            } {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn legacy_adoption_is_explicit_revisioned_and_preserves_policies_history_and_result_delivery()
{
    use crate::automation::*;
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ark_model::MockProvider::new(
        "test",
        ["Dependency done", "R1 COMPLETE", "R2 COMPLETE"]
            .into_iter()
            .map(|text| ark_model::Completion {
                content: text.into(),
                tool_calls: vec![],
                usage: Default::default(),
            })
            .collect(),
    ));
    let ark = Ark::builder()
        .store_root(dir.path())
        .provider(provider.clone())
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.create_project("legacy adoption", "").unwrap();
    let o = Orchestrator::builder(&ark, &project)
        .models(Arc::new(
            crate::Models::builder()
                .default_model("test")
                .provider("test", provider.clone())
                .build(),
        ))
        .build()
        .unwrap();
    o.spawn(AgentSpec::new("reviewer").model("test"))
        .await
        .unwrap();
    let dependency = o
        .run("reviewer", TaskSpec::new("Prepare input"))
        .await
        .unwrap();
    let destination = Session::start(ark.engine(), &project.id).unwrap();
    let book = RuleBook::new(&ark.engine().store);
    book.put(
        &ark_core::RuleSet::new("legacy-rule").rule(ark_core::Rule::new(
            "first",
            "Preserve the first rule version",
        )),
    )
    .unwrap();
    let mut task = TaskSpec::new("Original objective")
        .context("Original explicit context")
        .after([dependency.id.clone()])
        .budget(crate::Budget::default().steps(4).tokens(20_000))
        .report_to(destination.id().as_str());
    task.rules.push(RuleBinding::new(
        "legacy-rule",
        RuleScope::Task("original".into()),
    ));
    task.work = Some("original-client-work".into());
    let schedule = o
        .schedule("reviewer", task, When::every_minutes(5))
        .unwrap();
    o.own_agent(&schedule.id);
    let original = o
        .change_schedule(
            &schedule.id,
            ScheduleChange {
                retries: Some(0),
                ..Default::default()
            },
        )
        .unwrap();
    let before = ark
        .engine()
        .store
        .list_records(DEFINITION, Some(project.id.as_str()))
        .unwrap()
        .len();
    let projected = o.legacy_automation(&schedule.id).unwrap();
    assert_eq!(
        ark.engine()
            .store
            .list_records(DEFINITION, Some(project.id.as_str()))
            .unwrap()
            .len(),
        before
    );
    let adoption = LegacyAdoption {
        schedule: schedule.id.clone(),
        expected: projected.version,
        config: None,
    };
    let saved = o
        .save_automation_source(
            "adopt",
            None,
            None,
            projected.draft.clone(),
            Some(adoption.clone()),
        )
        .unwrap();
    let unchanged = o.schedule_record(&schedule.id).unwrap();
    assert_eq!(unchanged.target, None);
    assert_eq!(unchanged.next_ms, original.next_ms);
    assert_eq!(unchanged.version, original.version);
    assert!(unchanged.own_agent);
    let r1 = o.automation(&saved.automation).unwrap();
    assert!(r1.revision.digest.starts_with("v2:"));
    assert_eq!(
        r1.revision.legacy.as_ref().unwrap().rules[0].version,
        Some(1)
    );
    let grant = o
        .prepare_automation_activation(&saved.automation, 1)
        .await
        .unwrap();
    o.activate_automation(&saved.automation, &grant.id, &grant.digest)
        .unwrap();
    assert_eq!(
        o.save_automation_source("adopt", None, None, projected.draft.clone(), Some(adoption))
            .unwrap()
            .revision,
        1
    );
    let mut draft = projected.draft;
    if let StepAction::Agent { prompt, inputs, .. } = &mut draft.steps[0].action {
        *prompt = "Changed R2 objective".into();
        inputs.clear();
    }
    o.save_automation("draft-r2", Some(&saved.automation), Some(1), draft)
        .unwrap();
    book.put(
        &ark_core::RuleSet::new("legacy-rule")
            .rule(ark_core::Rule::new("second", "A later rule version")),
    )
    .unwrap();
    let current = o.schedule_record(&schedule.id).unwrap();
    assert!(current.own_agent);
    assert_eq!(current.retries, 0);
    assert_eq!(current.next_ms, original.next_ms);
    let first = o.tick_schedules(current.next_ms.unwrap()).await.remove(0);
    let completed = wait_automation(&o, &first, RunPhase::Completed).await;
    assert_eq!(completed.revision.number, 1);
    assert_eq!(completed.output.text, "R1 COMPLETE");
    let one = o
        .task(completed.steps[0].receipt.as_ref().unwrap())
        .unwrap();
    assert_eq!(one.objective, "Original objective");
    assert_eq!(one.context, "Original explicit context");
    assert_eq!(one.after, vec![dependency.id]);
    assert_eq!(one.budget.max_tokens, Some(20_000));
    assert_eq!(one.max_attempts, 1);
    assert_eq!(one.retry_delay_ms, Some(crate::schedule::RETRY_DELAY_MS));
    assert_eq!(one.work.as_deref(), Some("original-client-work"));
    assert_eq!(one.report_to, None);
    let rules = book.bindings(&RuleScope::Task(one.id)).unwrap();
    assert!(rules
        .iter()
        .any(|binding| binding.ruleset == "legacy-rule" && binding.version == Some(1)));
    wait_automation_delivery(&o, &first, true).await;
    let first_posts =
        ark_runtime::session::posts(ark.engine(), destination.id().as_str(), 0, 100).unwrap();
    assert_eq!(first_posts.len(), 1);
    assert_eq!(first_posts[0].1.text, "R1 COMPLETE");
    ark.engine()
        .store
        .transact_records::<_, OrchError>(project.id.as_str(), |tx| {
            let mut delivery = tx.get_record("automation_result_post", &first)?.unwrap();
            delivery["state"] = serde_json::json!("pending");
            tx.put_record("automation_result_post", &first, &delivery)?;
            Ok(())
        })
        .unwrap();
    o.inner.deliver_automation_reports().unwrap();
    assert_eq!(
        ark_runtime::session::posts(ark.engine(), destination.id().as_str(), 0, 100)
            .unwrap()
            .len(),
        1
    );
    let grant = o
        .prepare_automation_activation(&saved.automation, 2)
        .await
        .unwrap();
    o.activate_automation(&saved.automation, &grant.id, &grant.digest)
        .unwrap();
    let current = o.schedule_record(&schedule.id).unwrap();
    let second = o.tick_schedules(current.next_ms.unwrap()).await.remove(0);
    let completed = wait_automation(&o, &second, RunPhase::Completed).await;
    assert_eq!(completed.revision.number, 2);
    assert_eq!(completed.output.text, "R2 COMPLETE");
    let two = o
        .task(completed.steps[0].receipt.as_ref().unwrap())
        .unwrap();
    wait_automation_delivery(&o, &second, true).await;
    assert_eq!(two.objective, "Changed R2 objective");
    assert!(two.context.is_empty());
    assert_eq!(
        ark_runtime::session::posts(ark.engine(), destination.id().as_str(), 0, 100)
            .unwrap()
            .len(),
        2
    );
    assert_eq!(o.schedule_record(&schedule.id).unwrap().runs, 2);
    assert_eq!(o.schedule_record(&schedule.id).unwrap().failures, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn adopted_schedule_counts_root_failures_once_pauses_and_resumes_with_success() {
    use crate::automation::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    struct Credit(Arc<AtomicBool>);
    #[async_trait::async_trait]
    impl ark_model::Provider for Credit {
        fn describe(&self) -> String {
            "test credit".into()
        }
        fn host_controls_tools(&self) -> bool {
            true
        }
        async fn complete(
            &self,
            _: &str,
            _: &[ark_model::Message],
            _: &[ark_model::ToolDef],
        ) -> ark_model::Result<ark_model::Completion> {
            if self.0.load(Ordering::SeqCst) {
                Err(ark_model::ProviderError::OutOfCredit {
                    provider: "test".into(),
                    detail: "Test account has no credit".into(),
                })
            } else {
                Ok(ark_model::Completion {
                    content: "RECOVERED SUCCESS".into(),
                    tool_calls: vec![],
                    usage: Default::default(),
                })
            }
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let failing = Arc::new(AtomicBool::new(true));
    let provider = Arc::new(Credit(failing.clone()));
    let ark = Ark::builder()
        .store_root(dir.path())
        .provider(provider.clone())
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.create_project("failure settlement", "").unwrap();
    let models = Arc::new(
        crate::Models::builder()
            .default_model("test")
            .provider("test", provider)
            .build(),
    );
    let o = Orchestrator::builder(&ark, &project)
        .models(models.clone())
        .build()
        .unwrap();
    o.spawn(AgentSpec::new("reviewer").model("test"))
        .await
        .unwrap();
    let original = o
        .schedule("reviewer", TaskSpec::new("Report"), When::every_minutes(5))
        .unwrap();
    let original = o
        .change_schedule(
            &original.id,
            ScheduleChange {
                retries: Some(0),
                ..Default::default()
            },
        )
        .unwrap();
    let projected = o.legacy_automation(&original.id).unwrap();
    let saved = o
        .save_automation_source(
            "adopt",
            None,
            None,
            projected.draft,
            Some(LegacyAdoption {
                schedule: original.id.clone(),
                expected: projected.version,
                config: None,
            }),
        )
        .unwrap();
    let grant = o
        .prepare_automation_activation(&saved.automation, 1)
        .await
        .unwrap();
    o.activate_automation(&saved.automation, &grant.id, &grant.digest)
        .unwrap();
    for expected in 1..=3 {
        let schedule = o.schedule_record(&original.id).unwrap();
        let id = o.tick_schedules(schedule.next_ms.unwrap()).await.remove(0);
        wait_automation(&o, &id, RunPhase::Failed).await;
        let settled = o.schedule_record(&original.id).unwrap();
        assert_eq!(settled.failures, expected);
        assert_eq!(settled.counted.as_deref(), Some(id.as_str()));
        o.execute_automation_run(&id).unwrap();
        assert_eq!(o.schedule_record(&original.id).unwrap().failures, expected);
    }
    let paused = o.schedule_record(&original.id).unwrap();
    assert!(paused.paused);
    assert!(paused.next_ms.is_none());
    assert!(!o.automation(&saved.automation).unwrap().definition.enabled);
    let reopened = Orchestrator::builder(&ark, &project)
        .models(models)
        .build()
        .unwrap();
    assert_eq!(reopened.schedule_record(&original.id).unwrap().failures, 3);
    failing.store(false, Ordering::SeqCst);
    let grant = reopened
        .prepare_automation_activation(&saved.automation, 1)
        .await
        .unwrap();
    reopened
        .activate_automation(&saved.automation, &grant.id, &grant.digest)
        .unwrap();
    let resumed = reopened.schedule_record(&original.id).unwrap();
    assert_eq!(resumed.failures, 0);
    let id = reopened
        .tick_schedules(resumed.next_ms.unwrap())
        .await
        .remove(0);
    wait_automation(&reopened, &id, RunPhase::Completed).await;
    assert_eq!(reopened.schedule_record(&original.id).unwrap().failures, 0);
    assert_eq!(reopened.schedule_record(&original.id).unwrap().runs, 4);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn automation_result_outbox_keeps_the_answer_when_posting_fails_and_recovers_without_duplicates(
) {
    use crate::automation::*;
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ark_model::MockProvider::new(
        "test",
        vec![ark_model::Completion {
            content: "RETAINED BEFORE DELIVERY".into(),
            tool_calls: vec![],
            usage: Default::default(),
        }],
    ));
    let ark = Ark::builder()
        .store_root(dir.path())
        .provider(provider.clone())
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.create_project("outbox recovery", "").unwrap();
    let models = Arc::new(
        crate::Models::builder()
            .default_model("test")
            .provider("test", provider.clone())
            .build(),
    );
    let o = Orchestrator::builder(&ark, &project)
        .models(models.clone())
        .build()
        .unwrap();
    o.spawn(AgentSpec::new("reviewer").model("test"))
        .await
        .unwrap();
    let destination = Session::start(ark.engine(), &project.id).unwrap();
    let source = o
        .schedule(
            "reviewer",
            TaskSpec::new("Report").report_to(destination.id().as_str()),
            When::every_minutes(5),
        )
        .unwrap();
    let projected = o.legacy_automation(&source.id).unwrap();
    let saved = o
        .save_automation_source(
            "adopt",
            None,
            None,
            projected.draft,
            Some(LegacyAdoption {
                schedule: source.id,
                expected: projected.version,
                config: None,
            }),
        )
        .unwrap();
    let db = rusqlite::Connection::open(dir.path().join("meta.db")).unwrap();
    db.execute_batch(&format!("CREATE TRIGGER reject_result_post BEFORE INSERT ON events WHEN NEW.stream='session-posts/{}' BEGIN SELECT RAISE(ABORT,'injected post failure'); END;",destination.id())).unwrap();
    let run = o
        .admit_confirmed_test_run(
            &saved.automation,
            1,
            RunOrigin::Manual {
                request: "manual".into(),
            },
            Default::default(),
        )
        .await
        .unwrap();
    o.execute_automation_run(&run.id).unwrap();
    let completed = wait_automation(&o, &run.id, RunPhase::Completed).await;
    assert_eq!(completed.output.text, "RETAINED BEFORE DELIVERY");
    wait_automation_delivery(&o, &run.id, false).await;
    assert!(
        ark_runtime::session::posts(ark.engine(), destination.id().as_str(), 0, 100)
            .unwrap()
            .is_empty()
    );
    assert!(o.work_detail("person", &run.work).unwrap().entries[0]
        .delivery_error
        .is_some());
    assert_eq!(provider.calls.lock().unwrap().len(), 1);
    db.execute_batch("DROP TRIGGER reject_result_post").unwrap();
    ark.engine()
        .store
        .transact_records::<_, OrchError>(project.id.as_str(), |tx| {
            let mut report = tx.get_record("automation_result_post", &run.id)?.unwrap();
            report["retry_ms"] = serde_json::json!(0);
            tx.put_record("automation_result_post", &run.id, &report)?;
            Ok(())
        })
        .unwrap();
    let reopened = Orchestrator::builder(&ark, &project)
        .models(models)
        .build()
        .unwrap();
    assert_eq!(
        ark_runtime::session::posts(ark.engine(), destination.id().as_str(), 0, 100)
            .unwrap()
            .len(),
        1
    );
    ark.engine()
        .store
        .transact_records::<_, OrchError>(project.id.as_str(), |tx| {
            let mut report = tx.get_record("automation_result_post", &run.id)?.unwrap();
            report["state"] = serde_json::json!("pending");
            tx.put_record("automation_result_post", &run.id, &report)?;
            Ok(())
        })
        .unwrap();
    reopened.inner.deliver_automation_reports().unwrap();
    assert_eq!(
        ark_runtime::session::posts(ark.engine(), destination.id().as_str(), 0, 100)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(provider.calls.lock().unwrap().len(), 1);
    assert!(
        reopened.work_detail("person", &run.work).unwrap().entries[0]
            .delivery_error
            .is_none()
    );
}

#[tokio::test]
async fn legacy_cutover_refuses_changed_configuration_but_allows_cursor_progress() {
    use crate::automation::*;
    let (_dir, ark, project) = fixture();
    let o = Orchestrator::builder(&ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    o.spawn(AgentSpec::new("reviewer").model("test"))
        .await
        .unwrap();
    let schedule = o
        .schedule(
            "reviewer",
            TaskSpec::new("Original"),
            When::every_minutes(5),
        )
        .unwrap();
    let projected = o.legacy_automation(&schedule.id).unwrap();
    let saved = o
        .save_automation_source(
            "adopt",
            None,
            None,
            projected.draft,
            Some(LegacyAdoption {
                schedule: schedule.id.clone(),
                expected: projected.version,
                config: Some(projected.policies.source_digest),
            }),
        )
        .unwrap();
    let grant = o
        .prepare_automation_activation(&saved.automation, 1)
        .await
        .unwrap();
    o.pause_schedule(&schedule.id).unwrap();
    assert!(o
        .activate_automation(&saved.automation, &grant.id, &grant.digest)
        .is_err());
    assert!(o
        .prepare_automation_activation(&saved.automation, 1)
        .await
        .is_err());
    assert!(o.schedule_record(&schedule.id).unwrap().paused);
    assert!(o.schedule_record(&schedule.id).unwrap().target.is_none());
    o.resume_schedule(&schedule.id).unwrap();
    ark.engine()
        .store
        .transact_records::<_, OrchError>(project.id.as_str(), |tx| {
            let mut record = tx
                .get_record(crate::schedule::SCHEDULE, &schedule.id)?
                .unwrap();
            record["version"] = serde_json::json!(record["version"].as_u64().unwrap() + 1);
            record["missed"] = serde_json::json!(7);
            tx.put_record(crate::schedule::SCHEDULE, &schedule.id, &record)?;
            Ok(())
        })
        .unwrap();
    let grant = o
        .prepare_automation_activation(&saved.automation, 1)
        .await
        .unwrap();
    o.activate_automation(&saved.automation, &grant.id, &grant.digest)
        .unwrap();
    let current = o.schedule_record(&schedule.id).unwrap();
    assert_eq!(current.target.as_deref(), Some(saved.automation.as_str()));
    assert_eq!(current.missed, 7);
}

#[tokio::test]
async fn result_delivery_selects_due_records_before_limiting_the_queue() {
    let (_dir, ark, project) = fixture();
    let o = Orchestrator::builder(&ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    let destination = Session::start(ark.engine(), &project.id).unwrap();
    ark.engine()
        .store
        .transact_records::<_, OrchError>(project.id.as_str(), |tx| {
            for index in 0..102 {
                let id = format!("report-{index:03}");
                tx.insert_record(
                    "automation_result_post",
                    &id,
                    &serde_json::json!({
                        "run":id,"session":destination.id(),"from":"reviewer","title":"Result",
                        "text":format!("answer {index}"),"ok":true,"state":"pending","attempts":1,
                        "retry_ms":if index == 101 {0} else {u64::MAX / 2},"error":null
                    }),
                )?;
            }
            Ok(())
        })
        .unwrap();
    o.inner.deliver_automation_reports().unwrap();
    let posts =
        ark_runtime::session::posts(ark.engine(), destination.id().as_str(), 0, 100).unwrap();
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].1.text, "answer 101");
}

#[tokio::test]
async fn legacy_policy_refresh_is_explicit_revisioned_and_cannot_replace_an_active_source() {
    use crate::automation::*;
    let (_dir, ark, project) = fixture();
    let o = Orchestrator::builder(&ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    o.spawn(AgentSpec::new("reviewer").model("test"))
        .await
        .unwrap();
    let source = o
        .schedule(
            "reviewer",
            TaskSpec::new("Original"),
            When::every_minutes(5),
        )
        .unwrap();
    let first = o.legacy_automation(&source.id).unwrap();
    let saved = o
        .save_automation_source(
            "first",
            None,
            None,
            first.draft.clone(),
            Some(LegacyAdoption {
                schedule: source.id.clone(),
                expected: first.version,
                config: None,
            }),
        )
        .unwrap();
    o.change_schedule(
        &source.id,
        ScheduleChange {
            retries: Some(3),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(o
        .prepare_automation_activation(&saved.automation, 1)
        .await
        .is_err());
    let latest = o.legacy_automation(&source.id).unwrap();
    let adoption = LegacyAdoption {
        schedule: source.id.clone(),
        expected: latest.version,
        config: Some(latest.policies.source_digest),
    };
    let refreshed = o
        .save_automation_source(
            "refresh",
            Some(&saved.automation),
            Some(1),
            first.draft.clone(),
            Some(adoption.clone()),
        )
        .unwrap();
    assert_eq!(refreshed.revision, 2);
    assert_ne!(refreshed.digest, saved.digest);
    assert_eq!(
        o.automation(&saved.automation)
            .unwrap()
            .revision
            .legacy
            .unwrap()
            .max_attempts,
        4
    );
    let other = o
        .schedule("reviewer", TaskSpec::new("Other"), When::every_minutes(5))
        .unwrap();
    let other_projection = o.legacy_automation(&other.id).unwrap();
    assert!(o
        .save_automation_source(
            "wrong-source",
            Some(&saved.automation),
            Some(2),
            first.draft.clone(),
            Some(LegacyAdoption {
                schedule: other.id,
                expected: other_projection.version,
                config: None
            })
        )
        .is_err());
    assert!(o
        .save_automation_source(
            "duplicate",
            None,
            None,
            first.draft.clone(),
            Some(adoption.clone())
        )
        .is_err());
    let grant = o
        .prepare_automation_activation(&saved.automation, 2)
        .await
        .unwrap();
    o.activate_automation(&saved.automation, &grant.id, &grant.digest)
        .unwrap();
    assert_eq!(
        o.save_automation_source(
            "refresh",
            Some(&saved.automation),
            Some(1),
            first.draft.clone(),
            Some(adoption.clone())
        )
        .unwrap()
        .digest,
        refreshed.digest
    );
    assert!(o
        .save_automation_source(
            "active-refresh",
            Some(&saved.automation),
            Some(2),
            first.draft,
            Some(adoption)
        )
        .is_err());
}

#[tokio::test]
async fn context_messages_cannot_control_another_live_execution() {
    let (_dir, ark, project) = fixture();
    let o = Orchestrator::builder(&ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    o.spawn(AgentSpec::new("reader")).await.unwrap();
    let receipt = o
        .admit_work(
            crate::WorkTarget::Agent {
                name: "reader".into(),
            },
            TaskSpec::new("request"),
            "desktop",
            "context-owner",
        )
        .await
        .unwrap();
    let task = receipt.task.as_deref().unwrap();
    let mut record = o.task(task).unwrap();
    record.execution_owner = Some("another-runtime".into());
    record.pid = Some(std::process::id());
    record.pid_start = process::own_start();
    ark.engine()
        .store
        .put_record(
            TASK,
            task,
            project.id.as_str(),
            &serde_json::to_value(&record).unwrap(),
        )
        .unwrap();
    assert!(o
        .message("user", "reader", "plain")
        .unwrap_err()
        .to_string()
        .contains("another live runtime"));
    assert!(o
        .message_with_context("user", "reader", "selected", "{selection}")
        .unwrap_err()
        .to_string()
        .contains("another live runtime"));
    assert!(o.agent("reader").unwrap().inbox.is_empty());
}

#[tokio::test]
async fn an_agent_does_not_see_a_schedule_the_person_made_that_reports_into_its_chat() {
    let (_dir, ark, project) = fixture();
    let o = Orchestrator::builder(&ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    o.spawn(AgentSpec::new("ceo")).await.unwrap();
    let chat = o.start_thread("ceo").unwrap();
    let person = o
        .schedule(
            "ceo",
            TaskSpec::new("the person's").report_to(&chat),
            crate::When::every_minutes(120),
        )
        .unwrap();
    let view = crate::tools::Scheduling {
        orch: &o,
        chat: Some(&chat),
        zone: None,
        by: Some("ceo"),
        auto: true,
    };
    assert!(!view.sees(&person));
}

#[tokio::test]
async fn a_chat_whose_mode_cannot_be_read_is_offered_no_schedule_tools() {
    let (_dir, ark, project) = fixture();
    let o = Orchestrator::builder(&ark, &project)
        .models(test_models())
        .build()
        .unwrap();
    o.spawn(AgentSpec::new("ceo")).await.unwrap();
    let chat = o.start_thread("ceo").unwrap();
    let agent = o.agent("ceo").unwrap();
    let mut spec = TaskSpec::new("hello").thread(&chat);
    spec.person = true;
    let task = TaskRecord::queued(
        "t-1".into(),
        project.id.to_string(),
        "ceo".into(),
        &spec,
        crate::record::Origin::External,
        crate::Budget::default(),
        Vec::new(),
    );
    assert!(o
        .inner
        .schedule_offer(&agent.spec, &task, &agent)
        .is_some_and(|offer| offer.schedule));
    // The chat's record no longer reads.
    ark.engine()
        .store
        .put_session(
            &chat,
            &ProjectId(agent.scope.clone()),
            &serde_json::json!("not a session"),
        )
        .unwrap();
    assert!(o.inner.schedule_offer(&agent.spec, &task, &agent).is_none());
}
