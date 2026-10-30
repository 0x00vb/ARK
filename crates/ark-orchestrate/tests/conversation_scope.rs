use ark_model::{Completion, Message, Provider, ToolDef};
use ark_orchestrate::{ModelResolver, Models};
use ark_runtime::router::{CurrentModel, ModelRouter, Role};
use async_trait::async_trait;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct Bound {
    scope: String,
    seen: Arc<Mutex<Vec<String>>>,
}
#[async_trait]
impl Provider for Bound {
    fn for_conversation(&self, scope: &str) -> ark_model::Result<Option<Arc<dyn Provider>>> {
        Ok(Some(Arc::new(Self {
            scope: scope.into(),
            seen: self.seen.clone(),
        })))
    }
    fn describe(&self) -> String {
        "scoped:model".into()
    }
    async fn complete(
        &self,
        _: &str,
        _: &[Message],
        _: &[ToolDef],
    ) -> ark_model::Result<Completion> {
        self.seen.lock().unwrap().push(self.scope.clone());
        Ok(Completion::default())
    }
}

#[tokio::test]
async fn inherited_and_named_agent_models_rebind_without_sharing_the_parent_scope() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let base: Arc<dyn Provider> = Arc::new(Bound {
        scope: "unbound".into(),
        seen: seen.clone(),
    });
    let parent = base.for_conversation("chat/parent").unwrap().unwrap();
    let current = CurrentModel::new(ModelRouter::single(parent));
    let models = Models::builder()
        .follow(current)
        .provider("named", base)
        .build();
    for spec in [None, Some("named")] {
        let a = models.resolve("project/A", spec).await.unwrap();
        let b = models.resolve("project/B", spec).await.unwrap();
        for role in [Role::Reason, Role::Plan, Role::Critic] {
            a.router
                .provider(role)
                .complete("code", &[], &[])
                .await
                .unwrap();
            b.router
                .provider(role)
                .complete("code", &[], &[])
                .await
                .unwrap();
        }
    }
    let calls = seen.lock().unwrap();
    assert_eq!(calls.len(), 12);
    assert_eq!(
        calls
            .iter()
            .filter(|s| s.as_str() == "agent/project/A")
            .count(),
        6
    );
    assert_eq!(
        calls
            .iter()
            .filter(|s| s.as_str() == "agent/project/B")
            .count(),
        6
    );
    assert!(!calls.iter().any(|s| s == "chat/parent" || s == "unbound"));
}
