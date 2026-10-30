use ark_interact::{App, ChannelApprover, Start};
use ark_sdk::prelude::*;
use std::sync::Arc;

#[tokio::test]
async fn unavailable_saved_accounts_keep_the_app_and_model_recovery_open() {
    let root = tempfile::tempdir().unwrap();
    let secrets = Arc::new(ark_connect::MemorySecrets::default());
    let keys = ark_connect::ApiKeys::open(root.path(), secrets.clone());
    let account = keys
        .add_account("claude", &uuid::Uuid::new_v4().to_string(), "Work", "")
        .await
        .unwrap();
    let spec = format!("{}:sonnet", account.id);
    let open = || {
        let ark = Ark::builder()
            .store_root(root.path())
            .connections(true)
            .secrets(secrets.clone())
            .provider(Arc::new(ark_model::MockProvider::new("unused", vec![])))
            .build()
            .unwrap();
        let (approver, _) = ChannelApprover::channel();
        App::new("Account recovery")
            .ark(ark)
            .open(
                &Start {
                    model: Some(spec.clone()),
                    ..Default::default()
                },
                Arc::new(approver),
            )
            .unwrap()
            .interaction
    };
    let reconnecting = open();
    assert!(reconnecting.summary().model.starts_with(&account.id));
    drop(reconnecting);
    keys.accounts.begin_remove(&account.id).unwrap();
    keys.accounts.finish_remove(&account.id).unwrap();
    let mut removed = open();
    assert!(removed.summary().model.starts_with(&account.id));
    assert!(
        removed.switch_model(&spec).is_err(),
        "explicit missing-account selection is refused"
    );
    assert!(
        removed.switch_model("ollama:qwen3:4b-instruct").is_ok(),
        "a person can recover through the existing model selector"
    );
}
