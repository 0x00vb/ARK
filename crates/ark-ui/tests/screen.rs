//! A runtime other machines connect to never offers this machine's screen.

use std::sync::Arc;

use ark_interact::App;
use ark_sdk::prelude::ark_computer::{fake::FakeScreen, ComputerService};
use ark_sdk::prelude::ark_model::MockProvider;
use ark_sdk::prelude::*;
use ark_ui::access::Tokens;
use ark_ui::{Access, ArkUi};

fn app(dir: &std::path::Path, computer: bool) -> App {
    let mut b = Ark::builder()
        .store_root(dir)
        .provider(Arc::new(MockProvider::new("m", vec![])));
    if computer {
        b = b.computer_with(ComputerService::with_screen(Arc::new(FakeScreen::new())));
    }
    App::new("Screen test").ark(b.build().unwrap())
}

#[tokio::test(flavor = "multi_thread")]
async fn serving_with_tokens_refuses_an_app_that_uses_the_computer() {
    let dir = tempfile::tempdir().unwrap();
    let err = ArkUi::new(app(dir.path(), true))
        .access(Access::Tokens(Tokens::in_store(dir.path())))
        .serve_background("127.0.0.1:0")
        .await
        .err()
        .expect("refused")
        .to_string();
    assert!(err.contains("must not"), "{err}");
    // This machine only (the one-time token): allowed.
    let dir2 = tempfile::tempdir().unwrap();
    assert!(ArkUi::new(app(dir2.path(), true))
        .serve_background("127.0.0.1:0")
        .await
        .is_ok());
    // Tokens without the computer: allowed.
    let dir3 = tempfile::tempdir().unwrap();
    assert!(ArkUi::new(app(dir3.path(), false))
        .access(Access::Tokens(Tokens::in_store(dir3.path())))
        .serve_background("127.0.0.1:0")
        .await
        .is_ok());
}

#[tokio::test(flavor = "multi_thread")]
async fn local_human_recovery_requires_current_review_and_returns_unverified() {
    struct Key;
    impl ark_computer::SafetyKey for Key {
        fn load_or_create(&self) -> std::result::Result<[u8; 32], String> {
            Ok([31; 32])
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hazard.json");
    std::fs::write(&path, serde_json::to_vec(&serde_json::json!({"version":1,"hazard":{"kind":"submitted_unconfirmed","app":"Fixture Messages","window_identifier":"old-window","conversation_identifier":"private-conversation","composer_identifier":"composer","recipient_mac":"private-recipient-mac","created_at":1,"effect_id":"previous-effect"}})).unwrap()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let computer = ComputerService::with_screen(Arc::new(FakeScreen::new()))
        .message_safety(path, Arc::new(Key));
    let ark = Ark::builder()
        .store_root(dir.path())
        .provider(Arc::new(MockProvider::new("m", vec![])))
        .computer_with(computer.clone())
        .build()
        .unwrap();
    let (_host, served) = ArkUi::new(App::new("Recovery fixture").ark(ark))
        .serve_background("127.0.0.1:0")
        .await
        .unwrap();
    let client = reqwest::Client::new();
    let rpc = |method: &'static str, params: serde_json::Value| {
        client
            .post(format!("http://127.0.0.1:{}/rpc", served.port))
            .bearer_auth(&served.token)
            .json(&serde_json::json!({"method":method,"params":params}))
    };
    let hello: serde_json::Value = rpc("hello", serde_json::json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        hello["result"]["computer_hazard"]["kind"],
        "submitted_unconfirmed"
    );
    rpc("computer_stop", serde_json::json!({}))
        .send()
        .await
        .unwrap();
    let stopped: serde_json::Value = rpc("hello", serde_json::json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        stopped["result"]["computer_hazard"]["kind"],
        "submitted_unconfirmed"
    );
    let review: serde_json::Value = rpc("computer_review", serde_json::json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(review["ok"], true, "{review}");
    assert_eq!(review["result"]["kind"], "submitted_unconfirmed");
    assert!(!review.to_string().contains("private-conversation"));
    let refused: serde_json::Value = rpc(
        "computer_acknowledge",
        serde_json::json!({"token":review["result"]["token"]}),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(refused["ok"], false);
    assert!(computer.message_hazard().is_some());
    let result: serde_json::Value = rpc(
        "computer_acknowledge",
        serde_json::json!({"token":review["result"]["token"],"accepted_unverified":true}),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(result["result"]["verified"], false, "{result}");
    assert_eq!(result["result"]["retry_allowed"], false);
    assert_eq!(result["result"]["kind"], "submitted_unconfirmed");
    assert!(computer.message_hazard().is_none());
    let reloaded: serde_json::Value = rpc("hello", serde_json::json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(reloaded["result"]["computer_hazard"].is_null());
}
