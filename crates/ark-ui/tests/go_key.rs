use ark_interact::App;
use ark_sdk::prelude::*;
use ark_ui::ArkUi;
use serde_json::json;
use std::sync::Arc;

#[tokio::test]
async fn the_host_saves_go_keys_without_claiming_authentication_or_returning_the_secret() {
    let dir = tempfile::tempdir().unwrap();
    let model = Arc::new(ark_model::MockProvider::new("mock", vec![]));
    let secrets = Arc::new(ark_connect::MemorySecrets::default());
    let ark = Ark::builder()
        .store_root(dir.path())
        .provider(model.clone())
        .connections(true)
        .secrets(secrets.clone())
        .build()
        .unwrap();
    let host = ArkUi::new(App::new("Go key check").ark(ark))
        .open()
        .await
        .unwrap()
        .0;
    let saved = host
        .call(
            "api_key_save",
            json!({"provider":"opencode-go","key":"synthetic-local-go-key"}),
        )
        .await
        .unwrap();
    assert_eq!(saved["verified"], false);
    assert_eq!(saved["saved"], json!(["opencode-go"]));
    assert!(!saved.to_string().contains("synthetic-local-go-key"));
    let state = host.call("api_keys", json!({})).await.unwrap();
    assert_eq!(state["saved"], json!(["opencode-go"]));
    assert!(model.recorded().is_empty());
    host.call("api_key_forget", json!({"provider":"opencode-go"}))
        .await
        .unwrap();
    assert_eq!(
        host.call("api_keys", json!({})).await.unwrap()["saved"],
        json!([])
    );
    assert!(model.recorded().is_empty());
}
