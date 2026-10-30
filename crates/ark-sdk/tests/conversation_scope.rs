use std::sync::{Arc, Mutex};

use ark_model::{Completion, Message, Provider, ProviderError, ToolDef};
use ark_sdk::prelude::*;
use async_trait::async_trait;

#[derive(Clone)]
struct Scoped {
    seen: Arc<Mutex<Vec<String>>>,
    scope: Option<String>,
}

#[async_trait]
impl Provider for Scoped {
    fn for_conversation(&self, scope: &str) -> ark_model::Result<Option<Arc<dyn Provider>>> {
        if scope.is_empty() {
            return Err(ProviderError::Api {
                provider: "scoped".into(),
                detail: "empty scope".into(),
            });
        }
        Ok(Some(Arc::new(Self {
            scope: Some(scope.into()),
            ..self.clone()
        })))
    }
    fn describe(&self) -> String {
        "scoped model".into()
    }
    async fn complete(
        &self,
        _system: &str,
        _messages: &[Message],
        _tools: &[ToolDef],
    ) -> ark_model::Result<Completion> {
        self.seen
            .lock()
            .unwrap()
            .push(self.scope.clone().ok_or_else(|| ProviderError::Api {
                provider: "scoped".into(),
                detail: "unbound".into(),
            })?);
        Ok(Completion {
            content: "Hello.".into(),
            ..Default::default()
        })
    }
}

#[tokio::test]
async fn persisted_chats_and_both_router_tiers_keep_separate_scopes() {
    let directory = tempfile::tempdir().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let provider = Arc::new(Scoped {
        seen: seen.clone(),
        scope: None,
    });
    let ark = Ark::builder()
        .store_root(directory.path())
        .provider(provider)
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.create_project("p", "").unwrap();
    let a = ark.start_session(&project.id).unwrap();
    let b = ark.start_session(&project.id).unwrap();
    let restored = ark.open_session(a.id().as_str()).unwrap().unwrap();
    for chat in [&a, &b, &restored] {
        let router = ark.session_router(chat).await.unwrap();
        for role in [Role::Reason, Role::Plan, Role::Summarize, Role::Critic] {
            router
                .provider(role)
                .complete("code", &[], &[])
                .await
                .unwrap();
        }
    }
    let calls = seen.lock().unwrap();
    assert_eq!(calls.len(), 12);
    assert!(calls[..4].iter().all(|s| s == &calls[0]));
    assert!(calls[4..8].iter().all(|s| s == &calls[4]));
    assert!(calls[8..].iter().all(|s| s == &calls[0]));
    assert_ne!(calls[0], calls[4]);
}

#[tokio::test]
async fn one_shot_and_explicit_public_harnesses_bind_the_whole_run() {
    let directory = tempfile::tempdir().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let provider = Arc::new(Scoped {
        seen: seen.clone(),
        scope: None,
    });
    let ark = Ark::builder()
        .store_root(directory.path())
        .provider(provider)
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.create_project("p", "").unwrap();
    let profile = AgentProfile::default();
    let a = ark.harness_for_conversation("explicit/A").unwrap();
    let b = ark.harness_for_conversation("explicit/B").unwrap();
    let (first, second) = tokio::join!(
        a.ask(ark.engine(), &profile, &project.id, "hello", |_| {}),
        b.ask(ark.engine(), &profile, &project.id, "hello", |_| {})
    );
    first.unwrap();
    second.unwrap();
    assert!(ark.harness_for_conversation("").is_err());
    assert!(ark
        .harness()
        .ask(ark.engine(), &profile, &project.id, "hello", |_| {})
        .await
        .is_err());
    ark.ask(&project.id, "hello").await.unwrap();
    ark.ask(&project.id, "hello").await.unwrap();
    let calls = seen.lock().unwrap();
    assert!(calls.iter().any(|s| s == "explicit/A"));
    assert!(calls.iter().any(|s| s == "explicit/B"));
    let runs: std::collections::HashSet<_> =
        calls.iter().filter(|s| s.starts_with("run/")).collect();
    assert_eq!(runs.len(), 2);
}

#[tokio::test]
async fn distinct_tiers_are_bound_independently_and_errors_propagate() {
    let fast_seen = Arc::new(Mutex::new(Vec::new()));
    let strong_seen = Arc::new(Mutex::new(Vec::new()));
    let router = ModelRouter::new(
        Arc::new(Scoped {
            seen: fast_seen.clone(),
            scope: None,
        }),
        Arc::new(Scoped {
            seen: strong_seen.clone(),
            scope: None,
        }),
    );
    let bound = router.for_conversation("chat/A").unwrap();
    bound
        .provider(Role::Plan)
        .complete("code", &[], &[])
        .await
        .unwrap();
    bound
        .provider(Role::Reason)
        .complete("code", &[], &[])
        .await
        .unwrap();
    assert_eq!(*fast_seen.lock().unwrap(), ["chat/A"]);
    assert_eq!(*strong_seen.lock().unwrap(), ["chat/A"]);
    assert!(router.for_conversation("").is_err());
    assert!(router
        .provider(Role::Reason)
        .complete("code", &[], &[])
        .await
        .is_err());
}
