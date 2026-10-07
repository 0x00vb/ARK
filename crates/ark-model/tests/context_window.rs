//! Every model reports its input window, and every wrapper passes on the window of the model
//! it wraps (ADR 0050). A wrapper that forgot would leave its model on the smallest request
//! budget without any error, so each one is checked here.

use std::sync::Arc;

use ark_model::session::{Base, Delta, SessionBackend, SessionPolicy, SubscriptionRuntime};
use ark_model::{
    Anthropic, Capacity, Cli, CliModel, Completion, Limited, Limiter, Message, MockProvider,
    OpenAi, Provider, ToolDef,
};

#[test]
fn models_report_their_window() {
    let claude = CliModel::new(Cli::Claude, "sonnet");
    let codex = CliModel::new(Cli::Codex, "default");
    assert_eq!(Provider::context_window(&claude), Some(200_000));
    assert_eq!(Provider::context_window(&codex), Some(272_000));
    // The same window when they run as a vendor session's backend.
    assert_eq!(SessionBackend::context_window(&codex), Some(272_000));
    assert_eq!(
        Anthropic::new("claude-sonnet-4-5", "k").context_window(),
        Some(200_000)
    );
    // An OpenAI-compatible endpoint is unknown unless the app says.
    let any = OpenAi::new("http://localhost:1/v1", "local-model", "k");
    assert_eq!(Provider::context_window(&any), None);
    let said =
        OpenAi::new("http://localhost:1/v1", "local-model", "k").with_context_window(Some(64_000));
    assert_eq!(Provider::context_window(&said), Some(64_000));
    assert_eq!(MockProvider::new("m", vec![]).context_window(), None);
}

#[test]
fn a_limited_provider_passes_the_window_on() {
    let inner: Arc<dyn Provider> =
        Arc::new(MockProvider::new("m", vec![]).with_context_window(128_000));
    let limited = Limited::new(
        inner,
        Arc::new(Limiter::new("acct", Capacity::concurrent(1))),
    );
    assert_eq!(limited.context_window(), Some(128_000));
}

struct Backend(Option<u32>);

#[async_trait::async_trait]
impl SessionBackend for Backend {
    fn label(&self) -> String {
        "b".into()
    }
    fn model(&self) -> String {
        "b:m".into()
    }
    fn account(&self) -> String {
        "b".into()
    }
    fn context_window(&self) -> Option<u32> {
        self.0
    }
    async fn open(
        &self,
        _s: &str,
        _t: &[ToolDef],
        _m: &[Message],
    ) -> ark_model::Result<(String, Completion)> {
        unreachable!()
    }
    async fn resume(&self, _v: &str, _b: Base<'_>, _d: &Delta) -> ark_model::Result<Completion> {
        unreachable!()
    }
}

#[tokio::test]
async fn a_vendor_session_is_sized_by_its_rotation_limit() {
    // Each call is billed for the whole vendor thread, which rotates at 64k by default.
    let rt = SubscriptionRuntime::builder().build();
    let s = rt.session_with("a", Arc::new(Backend(Some(272_000)))).await;
    assert_eq!(s.context_window(), Some(64_000));
    // A smaller window than the limit is the window.
    let s = rt.session_with("b", Arc::new(Backend(Some(32_000)))).await;
    assert_eq!(s.context_window(), Some(32_000));
    let s = rt.session_with("c", Arc::new(Backend(None))).await;
    assert_eq!(s.context_window(), None);
    // Without reuse there is no vendor thread: the model's own window.
    let rt = SubscriptionRuntime::builder()
        .policy(SessionPolicy {
            reuse: false,
            ..Default::default()
        })
        .build();
    let s = rt.session_with("d", Arc::new(Backend(Some(272_000)))).await;
    assert_eq!(s.context_window(), Some(272_000));
}
