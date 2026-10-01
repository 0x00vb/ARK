//! The log of a running app tells an operator what each turn cost without recording what was
//! said: at `info` it has one line per turn with counts and timings, and no prompt, answer,
//! tool input or tool output. Its own test binary, because a logger is process-wide.

use std::sync::Arc;

use ark_interact::logging::{self, LogTarget};
use ark_interact::{App, ChannelApprover, Start, TurnControl};
use ark_sdk::prelude::ark_model::{Completion, MockProvider, ToolCall, Usage};
use ark_sdk::prelude::*;

struct Vault;
#[async_trait::async_trait]
impl Adapter for Vault {
    fn name(&self) -> &str {
        "vault"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![ActionDescriptor::new(
            "vault.read",
            "read an entry",
            RiskTier::Read,
        )]
    }
    async fn run(
        &self,
        _a: &str,
        _i: serde_json::Value,
        _c: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        Ok(ActionOutput::json(
            serde_json::json!({ "entry": "OUTPUT-SECRET-3" }),
        ))
    }
}

#[tokio::test]
async fn info_logs_count_the_turn_and_keep_its_content_out() {
    let dir = tempfile::tempdir().unwrap();
    std::env::set_var("ARK_LOG", "info");
    let log = logging::file_in(dir.path());
    assert!(logging::init(LogTarget::File(log.clone())));
    assert!(
        !logging::init(LogTarget::Stderr),
        "a second init is a no-op"
    );

    let mock = MockProvider::new("m", vec![]).with_fallback(|req| {
        if req.messages.iter().any(|m| m.tool_call_id.is_some()) {
            Completion {
                content: "ANSWER-SECRET-2 [inv:1].".into(),
                tool_calls: vec![],
                usage: Usage::measured(200, 10),
            }
        } else {
            Completion {
                content: String::new(),
                tool_calls: vec![ToolCall {
                    id: "c1".into(),
                    name: "vault.read".into(),
                    arguments: serde_json::json!({ "key": "INPUT-SECRET-4" }),
                }],
                usage: Usage::measured(150, 5),
            }
        }
    });
    let provider: Arc<dyn ark_model::Provider> = Arc::new(mock);
    let ark = Ark::builder()
        .store_root(dir.path())
        .tool(Arc::new(Vault))
        .profile(AgentProfile::builder().core_actions(["vault.read"]).build())
        .provider(provider)
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let (approver, _rx) = ChannelApprover::channel();
    let mut ix = App::new("Log test")
        .ark(ark)
        .open(&Start::default(), Arc::new(approver))
        .unwrap()
        .interaction;
    ix.run_turn(
        "QUESTION-SECRET-1: what is stored?",
        &TurnControl::new(),
        |_, _| {},
    )
    .await
    .result
    .unwrap();

    let text = std::fs::read_to_string(&log).unwrap();
    assert!(text.contains("turn finished"), "{text}");
    assert!(
        text.contains("model_calls=2") && text.contains("tool_calls=1"),
        "{text}"
    );
    for secret in [
        "QUESTION-SECRET-1",
        "ANSWER-SECRET-2",
        "OUTPUT-SECRET-3",
        "INPUT-SECRET-4",
    ] {
        assert!(
            !text.contains(secret),
            "{secret} leaked into the log:\n{text}"
        );
    }
}
