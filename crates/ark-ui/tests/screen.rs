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
