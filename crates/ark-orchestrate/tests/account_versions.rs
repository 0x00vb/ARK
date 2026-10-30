use ark_orchestrate::{ModelResolver, Models};
use ark_sdk::prelude::{ark_connect, ark_model};
use std::sync::Arc;

#[tokio::test]
async fn cached_agent_models_follow_reconnected_account_revisions_and_refuse_removal() {
    let root = tempfile::tempdir().unwrap();
    let keys =
        ark_connect::ApiKeys::open(root.path(), Arc::new(ark_connect::MemorySecrets::default()));
    ark_model::keys::set_source(Arc::new(keys.clone()));
    let account = keys
        .add_account("claude", &uuid::Uuid::new_v4().to_string(), "Work", "")
        .await
        .unwrap();
    let account = keys
        .accounts
        .activate(&account, Some("work@example.test"))
        .unwrap();
    let spec = format!("{}:sonnet", account.id);
    let models = Models::builder().default_model(&spec).build();
    let following = models.resolve("following", None).await.unwrap();
    let explicit = models.resolve("explicit", Some(&spec)).await.unwrap();
    assert!(models.is_current(&following));
    assert!(models.is_current(&explicit));
    let next = keys.accounts.begin_reconnect(&account.id).unwrap();
    assert!(!models.is_current(&following));
    assert!(!models.is_current(&explicit));
    assert!(models.resolve("following", None).await.is_err());
    keys.accounts
        .activate(&next, Some("work@example.test"))
        .unwrap();
    let renewed = models.resolve("following", None).await.unwrap();
    assert!(models.is_current(&renewed));
    assert_ne!(
        renewed.session.as_ref().unwrap().account_revision(),
        following.session.as_ref().unwrap().account_revision()
    );
    keys.accounts.begin_remove(&account.id).unwrap();
    keys.accounts.finish_remove(&account.id).unwrap();
    assert!(!models.is_current(&renewed));
    assert!(models.resolve("following", None).await.is_err());
    let api = keys
        .add_account(
            "opencode-go",
            &uuid::Uuid::new_v4().to_string(),
            "API",
            "first-fixture-key",
        )
        .await
        .unwrap();
    let api_spec = format!("{}:glm-5.3-flash", api.id);
    let api_models = Models::builder().default_model(&api_spec).build();
    let api_following = api_models.resolve("api-following", None).await.unwrap();
    let api_explicit = api_models
        .resolve("api-explicit", Some(&api_spec))
        .await
        .unwrap();
    assert!(api_models.is_current(&api_following));
    assert!(api_models.is_current(&api_explicit));
    keys.accounts.begin_reconnect(&api.id).unwrap();
    keys.accounts
        .replace_key(&api.id, "second-fixture-key")
        .unwrap();
    assert!(!api_models.is_current(&api_following));
    assert!(!api_models.is_current(&api_explicit));
    let fresh_following = api_models.resolve("api-following", None).await.unwrap();
    let fresh_explicit = api_models
        .resolve("api-explicit", Some(&api_spec))
        .await
        .unwrap();
    assert!(api_models.is_current(&fresh_following));
    assert!(api_models.is_current(&fresh_explicit));
}
