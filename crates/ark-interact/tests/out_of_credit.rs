//! A model whose account is out of credit stops the turn once, with the provider's words and
//! what to do; a named endpoint's model ids never collide with built-in provider names.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use ark_interact::{App, ChannelApprover, Interaction, Start, TurnControl};
use ark_sdk::prelude::ark_model::{
    self, Completion, Endpoint, Message, Provider, ProviderError, ToolDef,
};
use ark_sdk::prelude::*;

struct Broke(Arc<AtomicUsize>);
#[async_trait::async_trait]
impl Provider for Broke {
    async fn complete(
        &self,
        _s: &str,
        _m: &[Message],
        _t: &[ToolDef],
    ) -> ark_model::Result<Completion> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(ProviderError::OutOfCredit {
            provider: "t-credit".into(),
            detail: "Your balance is used up. Add credit to continue.".into(),
        })
    }
    fn describe(&self) -> String {
        "t-credit auto".into()
    }
}

fn open(dir: &std::path::Path, provider: Arc<dyn Provider>) -> Interaction {
    let ark = Ark::builder()
        .store_root(dir)
        .provider(provider)
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let (approver, _rx) = ChannelApprover::channel();
    App::new("Test")
        .ark(ark)
        .open(&Start::default(), Arc::new(approver))
        .unwrap()
        .interaction
}

#[tokio::test]
async fn out_of_credit_stops_the_turn_once_with_what_to_do() {
    ark_model::endpoints::register(
        Endpoint::new("t-credit", "http://127.0.0.1:9/v1").label("Router"),
    )
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let mut ix = open(dir.path(), Arc::new(Broke(calls.clone())));
    let end = ix.run_turn("hello", &TurnControl::new(), |_, _| {}).await;
    assert_eq!(calls.load(Ordering::SeqCst), 1, "not retried");
    let e = end.result.as_ref().err().unwrap();
    assert_eq!(
        e.to_string(),
        "Your balance is used up. Add credit to continue."
    );
    let o = end.out_of_credit().expect("out of credit");
    assert_eq!(o.provider, "t-credit");
    assert_eq!(
        o.hint,
        "Add credit to your Router account, or pick another model."
    );
    ark_model::endpoints::unregister("t-credit");
}

#[tokio::test]
async fn a_named_endpoint_model_called_claude_is_not_claude_code() {
    ark_model::endpoints::register(Endpoint::new("t-spec", "http://127.0.0.1:9/v1").key("k"))
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let mut ix = open(dir.path(), Arc::new(Broke(calls)));
    assert_eq!(ix.switch_model("t-spec:claude").unwrap(), "t-spec claude");
    // A bare model keeps the endpoint.
    assert_eq!(ix.switch_model("auto").unwrap(), "t-spec auto");
    ark_model::endpoints::unregister("t-spec");
}
