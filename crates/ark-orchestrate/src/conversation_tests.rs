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
    let o = Orchestrator::builder(&ark, &project).build().unwrap();
    let task = source(&o, &["agents.create", "files.read"], None).await;
    let (a, b) = tokio::join!(
        o.inner
            .create_colleague("source", &task, "colleague", "review", "read carefully"),
        o.inner
            .create_colleague("source", &task, "colleague", "review", "read carefully")
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
        .create_colleague("source", &another, "colleague", "replace", "overwrite")
        .await
        .is_err());
    assert_eq!(o.agent("colleague").unwrap().spec.role, "review");
}

#[tokio::test]
async fn conversation_creation_respects_allowlists_and_root_quota_after_reload() {
    let (_dir, ark, project) = fixture();
    let o = Orchestrator::builder(&ark, &project).build().unwrap();
    let task = source(&o, &["files.read"], None).await;
    assert!(o
        .inner
        .create_colleague("source", &task, "forbidden", "review", "")
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
        .create_colleague("source", &task, "first", "review", "")
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
        .create_colleague("first", &descendant, "second", "review", "")
        .await
        .is_err());
    assert_eq!(
        o.inner
            .create_colleague("source", &task, "first", "review", "")
            .await
            .unwrap()
            .scope,
        a.scope
    );
    let reloaded = Orchestrator::builder(&ark, &project).build().unwrap();
    assert_eq!(reloaded.agent("first").unwrap().scope, a.scope);
    assert!(reloaded
        .inner
        .create_colleague("source", &task, "second", "review", "")
        .await
        .is_err());
}

#[tokio::test]
async fn conversation_start_is_independent_idempotent_and_scoped() {
    let (_dir, ark, project) = fixture();
    let o = Orchestrator::builder(&ark, &project).build().unwrap();
    let task = source(&o, &["*"], None).await;
    o.inner
        .create_colleague("source", &task, "colleague", "review", "")
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
    let o = Orchestrator::builder(&ark, &project).build().unwrap();
    let task = source(&o, &["*"], Some(crate::Budget::default().delegations(1))).await;
    o.inner
        .create_colleague("source", &task, "colleague", "review", "")
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
    let base = Orchestrator::builder(&ark, &project).build().unwrap();
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
                .create_colleague("source", &task, "colleague", "review", "")
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refused_concurrent_board_claims_leave_no_durable_tasks_to_recover() {
    let (_dir, ark, project) = fixture();
    let o = Orchestrator::builder(&ark, &project).build().unwrap();
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
    }));
    let weather = ark_runtime::catalog::matching_actions(&registry,
        "Task t-weather delegated by lead (from its task t-lead).\n\n== OBJECTIVE ==\nGet the current temperature in Paris and in Tokyo.", 2);
    assert!(
        weather.iter().any(|name| name == "weather.current"),
        "{weather:?}"
    );
    assert!(
        !weather.iter().any(|name| name == crate::START),
        "{weather:?}"
    );
    let customer = ark_runtime::catalog::matching_actions(&registry,
        "Task t-customer delegated by lead (from its task t-lead).\n\n== OBJECTIVE ==\nFind the customer Ada Lovelace: her tier, and the total of all her orders.", 2);
    assert_eq!(customer.len(), 2);
    assert!(
        customer.iter().all(|name| name.starts_with("crm.")),
        "{customer:?}"
    );
}

#[tokio::test]
async fn client_sends_keep_device_identity_across_restart_and_reject_conflicts() {
    let (_dir, ark, project) = fixture();
    let o = Orchestrator::builder(&ark, &project).build().unwrap();
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
