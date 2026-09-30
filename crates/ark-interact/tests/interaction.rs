//! The interaction layer end to end with a mock model: a turn runs, its trace is stored,
//! the transcript restores activity, commands return reports, sessions and projects switch.

use std::sync::Arc;

use ark_interact::{ActivityKind, App, ChannelApprover, Effect, Interaction, Start, TurnControl};
use ark_sdk::prelude::ark_model::{Completion, MockProvider, ToolCall, Usage};
use ark_sdk::prelude::*;

struct Notes;
#[async_trait::async_trait]
impl Adapter for Notes {
    fn name(&self) -> &str {
        "notes"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![ActionDescriptor::new(
            "notes.read",
            "read the notes",
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
            serde_json::json!({ "text": "remember the locket" }),
        ))
    }
}

fn open(dir: &std::path::Path) -> Interaction {
    let mock = MockProvider::new("m", vec![]).with_fallback(|req| {
        if req.messages.iter().any(|m| m.tool_call_id.is_some()) {
            Completion {
                content: "The notes mention a locket [inv:1].".into(),
                tool_calls: vec![],
                usage: Usage::measured(200, 10).with_cached(120),
            }
        } else {
            let call = ToolCall {
                id: "c1".into(),
                name: "notes.read".into(),
                arguments: serde_json::json!({}),
            };
            Completion {
                content: String::new(),
                tool_calls: vec![call],
                usage: Usage::measured(150, 5),
            }
        }
    });
    let provider: Arc<dyn ark_model::Provider> = Arc::new(mock);
    let ark = Ark::builder()
        .store_root(dir)
        .tool(Arc::new(Notes))
        .profile(AgentProfile::builder().core_actions(["notes.read"]).build())
        .provider(provider)
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let (approver, _rx) = ChannelApprover::channel();
    App::new("Test")
        .ark(ark)
        .labels(
            ark_interact::ActivityLabels::new().label("notes.read", |_| "Reading the notes".into()),
        )
        .open(&Start::default(), Arc::new(approver))
        .unwrap()
        .interaction
}

#[tokio::test]
async fn a_turn_is_described_traced_stored_and_restored() {
    let dir = tempfile::tempdir().unwrap();
    let mut ix = open(dir.path());
    let ctl = TurnControl::new();
    let mut labels = Vec::new();
    let end = ix
        .run_turn("what do the notes say", &ctl, |_, a| {
            if let Some(a) = a {
                labels.push((a.kind, a.label.clone()));
            }
        })
        .await;
    let out = end.result.unwrap();
    assert!(out.answer.contains("locket"));
    assert_eq!(end.index, Some(0));
    assert!(end.footer.iter().any(|f| f == "1 tool"), "{:?}", end.footer);
    assert!(labels.contains(&(ActivityKind::Tool, "Reading the notes".into())));

    // The trace is interpreted from events, with the call's context contribution.
    let trace = ix.trace(None).unwrap();
    assert_eq!(trace.calls.len(), 1);
    assert!(trace.calls[0].context_tokens.unwrap() > 0);
    assert_eq!(trace.calls[0].seen_by_requests, 1);
    assert_eq!(trace.cached_tokens, 120);

    // Reopened in a new interaction, the transcript restores the same activity from the store.
    let sid = ix.session().id().as_str().to_string();
    drop(ix);
    let mut again = open(dir.path());
    assert!(again.resume(&sid).unwrap());
    let turns = again.transcript(None, 10).unwrap();
    assert_eq!(turns.len(), 1);
    assert!(turns[0].has_trace);
    assert!(turns[0]
        .activity
        .iter()
        .any(|a| a.label == "Reading the notes"));
    assert!(
        again.trace(Some(0)).is_some(),
        "an old turn's trace is read back from the store"
    );
}

#[tokio::test]
async fn commands_return_structured_reports_and_effects() {
    let dir = tempfile::tempdir().unwrap();
    let mut ix = open(dir.path());
    let ctl = TurnControl::new();
    ix.run_turn("read the notes", &ctl, |_, _| {})
        .await
        .result
        .unwrap();

    let ctx = ix.execute("/context").await;
    assert_eq!(ctx.report.data.as_ref().unwrap().view, "context");
    assert!(ctx.report.plain_text().contains("Kept out of the request"));

    let usage = ix.execute("/usage").await.report;
    assert!(
        usage.plain_text().contains("cached input"),
        "{}",
        usage.plain_text()
    );

    let trace = ix.execute("/trace 1").await.report;
    assert!(
        trace.plain_text().contains("#1 notes.read"),
        "{}",
        trace.plain_text()
    );

    assert_eq!(ix.execute("/clear").await.effect, Some(Effect::ClearView));
    assert_eq!(
        ix.execute("hello").await.effect,
        Some(Effect::Submit("hello".into()))
    );
    assert!(ix
        .execute("/nope")
        .await
        .report
        .plain_text()
        .contains("Unknown command"));
    let help = ix.execute("/help").await.report.plain_text();
    assert!(help.contains("/context") && help.contains("/rename"));

    ix.execute("/rename Locket hunt").await;
    assert_eq!(ix.session().record().title, "Locket hunt");
    let first = ix.session().id().clone();
    ix.execute("/project other").await;
    assert_eq!(ix.project().name, "other");
    assert_ne!(ix.session().id(), &first);
    let all = ix.sessions(true).unwrap();
    assert!(all.iter().any(|s| s.title == "Locket hunt" && !s.current));
}

#[tokio::test]
async fn late_steering_comes_back_undelivered() {
    let dir = tempfile::tempdir().unwrap();
    let mut ix = open(dir.path());
    let ctl = TurnControl::new();
    let steer = ctl.steer.clone();
    let end = ix
        .run_turn("read the notes", &ctl, |ev, _| {
            if matches!(ev, HarnessEvent::Answer { .. }) {
                steer.push("and the door?");
            }
        })
        .await;
    assert_eq!(end.undelivered, vec!["and the door?"]);
}
