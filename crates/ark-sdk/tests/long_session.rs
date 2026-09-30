//! Regression: terminal history is not LLM context. A 500-turn session must send the model
//! about the same prompt on turn 500 as on turn 10, while the stored transcript grows without
//! bound. Run with `--nocapture` to print the table recorded in docs/CLI.md.

use std::sync::Arc;

use ark_sdk::prelude::*;
use ark_sdk::prelude::ark_model::{estimate_tokens, Completion, MockProvider, ToolCall, Usage};

struct Notes;
#[async_trait::async_trait]
impl Adapter for Notes {
    fn name(&self) -> &str {
        "notes"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![ActionDescriptor::new("notes.read", "read the notes", RiskTier::Read)]
    }
    async fn run(&self, _a: &str, _i: serde_json::Value, _c: &RunCtx<'_>) -> ark_tools::Result<ActionOutput> {
        // A bulky tool result: distillation must keep it out of the prompt.
        Ok(ActionOutput::json(serde_json::json!({ "text": "the locket, the key, the door. ".repeat(200) })))
    }
}

fn request_tokens(r: &ark_model::mock::Recorded) -> u32 {
    estimate_tokens(&r.system) + r.messages.iter().map(|m| estimate_tokens(&m.content) + 4).sum::<u32>()
}

#[tokio::test]
async fn five_hundred_turns_keep_a_flat_prompt() {
    let dir = tempfile::tempdir().unwrap();
    // Every turn: one tool call, then a long answer (about 1,500 characters).
    let mock = Arc::new(MockProvider::new("m", vec![]).with_fallback(|req| {
        let has_result = req.messages.iter().any(|m| m.tool_call_id.is_some());
        if has_result {
            Completion { content: "Here is a long, careful answer about the manuscript. ".repeat(28), tool_calls: vec![], usage: Usage::default() }
        } else {
            let call = ToolCall { id: "c".into(), name: "notes.read".into(), arguments: serde_json::json!({}) };
            Completion { content: String::new(), tool_calls: vec![call], usage: Usage::default() }
        }
    }));
    let provider: Arc<dyn ark_model::Provider> = mock.clone();
    let ark = Ark::builder()
        .store_root(dir.path())
        .tool(Arc::new(Notes))
        .profile(AgentProfile::builder().core_actions(["notes.read"]).build())
        .provider(provider)
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.create_project("long", "").unwrap();
    let mut session = ark.start_session(&project.id).unwrap();

    let checkpoints = [1usize, 10, 50, 100, 250, 500];
    let mut rows = Vec::new();
    let mut transcript_tokens: u64 = 0;
    let mut peaks = Vec::new();
    for turn in 1..=500 {
        let before = mock.call_count();
        let q = format!("turn {turn}: keep going with the revision and tell me what changed in detail");
        let out = ark.ask_session(&mut session, &q, TurnOptions::default(), |_| {}).await.unwrap();
        transcript_tokens += (estimate_tokens(&q) + estimate_tokens(&out.answer)) as u64;
        let recorded = mock.recorded();
        let turn_peak = recorded[before..].iter().map(request_tokens).max().unwrap();
        peaks.push(turn_peak);
        if checkpoints.contains(&turn) {
            let conversation = session.conversation_digest().map(|d| estimate_tokens(&d)).unwrap_or(0);
            rows.push((turn, turn_peak, conversation, transcript_tokens));
        }
    }

    println!("\n| turn | largest request (tokens) | carried conversation | full transcript |");
    println!("|---:|---:|---:|---:|");
    for (turn, peak, conv, transcript) in &rows {
        println!("| {turn} | {peak} | {conv} | {transcript} |");
    }

    // The carry-over window fills after about a dozen turns; from then on the prompt is flat.
    let at_50 = peaks[49];
    let at_500 = peaks[499];
    assert!(at_500 <= at_50 + at_50 / 50, "prompt must plateau: turn 50 = {at_50}, turn 500 = {at_500}");
    let (_, _, _, transcript_at_500) = rows.last().unwrap();
    assert!(*transcript_at_500 > 100 * at_500 as u64, "the transcript is far larger than any prompt");
    assert!(at_500 < 6000, "stays under the default budget: {at_500}");
    assert_eq!(session.turn_count(), 500);
    assert_eq!(session.transcript(ark.engine(), None).unwrap().len(), 500, "nothing was thrown away");
}
