//! ADR 0121 on a real model. Ignored: it calls a live model (`claude:haiku` is enough).
//!
//! ```text
//! ARK_LIVE_MODEL=claude:haiku ARK_LIVE_OUT=<folder> \
//!   cargo test -p ark-orchestrate --test live_images -- --ignored --nocapture
//! ```
//!
//! A person's task on a named agent carries one real PNG (a red circle with the black text 42).
//! It passes when the answer mentions the circle or the number. The agent runs on the app's
//! model as in the apps: a managed session of its own. The answer, tokens and calls are
//! printed and written to `RESULTS.md` in `ARK_LIVE_OUT` (default: a temporary folder).

use std::sync::Arc;
use std::time::Duration;

use ark_orchestrate::*;
use ark_sdk::prelude::*;

const PICTURE: &[u8] = include_bytes!("fixtures/circle42.png");
const OBJECTIVE: &str = "What is in this picture? Answer in one sentence.";

fn model() -> Arc<dyn ark_model::Provider> {
    let spec = std::env::var("ARK_LIVE_MODEL").unwrap_or_else(|_| "claude:haiku".into());
    let (p, m) = spec.split_once(':').unwrap_or((spec.as_str(), ""));
    ark_model::build(p, (!m.is_empty()).then(|| m.to_string())).expect("the model")
}

#[tokio::test]
#[ignore = "calls a live model"]
async fn a_picture_sent_to_a_named_agent_is_seen_by_the_model() {
    let dir = tempfile::tempdir().unwrap();
    let ark = Ark::builder()
        .store_root(dir.path().join("store"))
        .provider(model())
        .planner(false)
        .critic(false)
        .build()
        .unwrap();
    let project = ark.find_or_create_project("p", "").unwrap();
    let folder = dir.path().join("project");
    std::fs::create_dir_all(&folder).unwrap();
    ark.engine().set_workspace(&project.id, &folder).unwrap();
    std::fs::write(folder.join("circle42.png"), PICTURE).unwrap();
    let o = Orchestrator::builder(&ark, &project).build().unwrap();
    o.spawn(AgentSpec::new("Writer")).await.unwrap();
    let spec = TaskSpec {
        person: true,
        ..TaskSpec::new(OBJECTIVE).images(["circle42.png"])
    };
    let id = o.submit("Writer", spec).unwrap();
    let task = tokio::time::timeout(Duration::from_secs(300), o.wait(&id))
        .await
        .expect("finished in 5 minutes")
        .unwrap();
    let answer = task.result.clone().unwrap_or_default();
    let lower = answer.to_lowercase();
    let pass = task.state == TaskState::Done
        && (lower.contains("circle") || lower.contains("42") || lower.contains("red"));
    let report = format!(
        "# Live check: a picture on a task (ADR 0121)\n\n| Item | Value |\n|---|---|\n| Model | {} |\n| Picture | circle42.png ({} bytes): red circle, black text 42 |\n| Objective | {OBJECTIVE} |\n| Result | {} |\n| State | {:?} |\n| Prompt tokens | {} (cached {}) |\n| Completion tokens | {} |\n| Model calls | {} |\n| Usage measured | {} |\n| Images on the task | {:?} |\n\nAnswer:\n\n> {}\n\nError: {:?}\n",
        std::env::var("ARK_LIVE_MODEL").unwrap_or_else(|_| "claude:haiku".into()),
        PICTURE.len(),
        if pass { "PASS" } else { "FAIL" },
        task.state,
        task.usage.prompt_tokens,
        task.usage.cached_tokens,
        task.usage.completion_tokens,
        task.usage.model_calls,
        task.usage.measured,
        task.images.iter().map(|i| i.path.as_str()).collect::<Vec<_>>(),
        answer.replace('\n', "\n> "),
        task.error,
    );
    println!("{report}");
    let out = std::env::var("ARK_LIVE_OUT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| dir.path().to_path_buf());
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("RESULTS.md"), &report).unwrap();
    assert!(pass, "the answer does not describe the picture: {answer}");
}
