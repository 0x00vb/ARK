//! # agent-team
//!
//! A coordinator and two readers, built from ARK's orchestration primitives:
//!
//! ```text
//! lead (coordinator)
//!  ├─ agents.delegate → reader-a: read the spring review   ┐ in parallel,
//!  ├─ agents.delegate → reader-b: read the summer review   ┘ each in its own context
//!  └─ combine both results, store a diagram of the themes
//! ```
//!
//! Each agent runs on the model its environment variable names, so the same program runs on
//! subscriptions, APIs or a local model:
//!
//! | Variable | Default |
//! |---|---|
//! | `ARK_LEAD_MODEL` | `claude:sonnet` |
//! | `ARK_READER_A_MODEL` | `codex` |
//! | `ARK_READER_B_MODEL` | `claude:haiku` |
//! | `ARK_TEAM_MODEL` | (unset) one model for all three |
//!
//! ```text
//! cargo run -p agent-team
//! ARK_TEAM_MODEL=ollama:qwen3:4b-instruct cargo run -p agent-team
//! ```
//!
//! It prints the agents' activity as it happens, the result, the agent and task graph, each
//! agent's model session, and the graph as a Mermaid diagram. The store stays in a temporary
//! directory (or `ARK_TEAM_STORE`) for inspection.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ark_interact::orchestration::{event_line, graph_report};
use ark_orchestrate::{AgentSpec, Budget, EventKind, Orchestrator, TaskSpec};
use ark_sdk::prelude::*;

/// `doc.read`: the text of a stored document. The only app tool; readers may use it, the
/// lead may not.
struct Docs;

#[async_trait::async_trait]
impl Adapter for Docs {
    fn name(&self) -> &str {
        "doc"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![
            ActionDescriptor::new("doc.read", "read a stored document's text", RiskTier::Read)
                .reusable()
                .with_input_schema(ark_tools::artifact_input_schema("art:<id> of the document")),
        ]
    }
    async fn run(
        &self,
        action: &str,
        input: serde_json::Value,
        ctx: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        let id = ark_tools::arg_artifact(action, &input)?;
        let text = String::from_utf8_lossy(&ctx.blobs.read(&id)?).to_string();
        Ok(ActionOutput::json(serde_json::json!({ "text": text })))
    }
}

/// The document text is the evidence: give it to the model whole (it is short).
fn docs_distiller() -> Distiller {
    Arc::new(|action: &str, out: &serde_json::Value| {
        (action == "doc.read").then(|| (out["text"].as_str().unwrap_or("").to_string(), Vec::new()))
    })
}

fn model(var: &str, default: &str) -> String {
    std::env::var("ARK_TEAM_MODEL")
        .or_else(|_| std::env::var(var))
        .unwrap_or_else(|_| default.to_string())
}

const LEAD: &str = "You lead a review of two documents. Give each document to one reader with \
agents.delegate, passing the document's artifact id in `artifacts` and saying what to report. \
Delegate both in the same reply so the readers work in parallel. When both results are back, \
store one flowchart of the themes the two documents share with diagram.create, then answer \
with the comparison.";

const READER: &str = "You read one document and report on it. Read it with doc.read, then \
answer with its key figures, what went well, what did not, and the plan it states. Report \
only what the document says.";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let root = std::env::var("ARK_TEAM_STORE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::temp_dir().join(format!("ark-agent-team-{}", std::process::id()))
        });
    let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("data");

    // Agents allowed `doc.read` get it preloaded; the others never see it.
    let profile = AgentProfile::builder()
        .distillers(DistillerSet::default().with(docs_distiller()))
        .core_actions(["doc.read"])
        .build();
    let ark = Ark::builder()
        .store_root(&root)
        .tool(Arc::new(Docs))
        .profile(profile)
        .planner(false)
        .critic(false)
        .build()?;
    let project = ark.create_project("reviews", "compare two quarterly reviews")?;
    let spring = ark
        .engine()
        .ingest_path(&project.id, &data.join("spring-review.md"))?;
    let summer = ark
        .engine()
        .ingest_path(&project.id, &data.join("summer-review.md"))?;

    // The project's writing standard: stored once, versioned, applied per task by relevance.
    let rules = RuleSet::parse(
        "review-writing",
        &std::fs::read_to_string(data.join("rules.md"))?,
    );
    let rules = ark.rule_book().put(&rules)?;
    ark.rule_book().bind(&RuleBinding::new(
        &rules.name,
        RuleScope::Project(project.id.to_string()),
    ))?;

    let orch = Orchestrator::builder(&ark, &project).build()?;
    let (lead_m, a_m, b_m) = (
        model("ARK_LEAD_MODEL", "claude:sonnet"),
        model("ARK_READER_A_MODEL", "codex"),
        model("ARK_READER_B_MODEL", "claude:haiku"),
    );
    orch.spawn(
        AgentSpec::new("lead")
            .role("coordinates the review")
            .instructions(LEAD)
            .model(&lead_m)
            .tools(Vec::<String>::new())
            .delegates_to(["reader-a", "reader-b"])
            .diagrams(true),
    )
    .await?;
    for (name, m) in [("reader-a", &a_m), ("reader-b", &b_m)] {
        orch.spawn(
            AgentSpec::new(name)
                .role("reads one document and reports its facts")
                .instructions(READER)
                .model(m)
                .tools(["doc.read"])
                .parent("lead")
                .budget(Budget::default().steps(4)),
        )
        .await?;
    }
    println!("store: {}", root.display());
    println!("models: lead {lead_m} · reader-a {a_m} · reader-b {b_m}\n");

    // Follow the run as it happens, from the orchestrator's own events.
    let mut rx = orch.subscribe();
    let printer = tokio::spawn(async move {
        let mut owners = std::collections::HashMap::new();
        while let Ok(ev) = rx.recv().await {
            if let EventKind::TaskCreated { task, owner, .. } = &ev.kind {
                owners.insert(task.clone(), owner.clone());
            }
            // The task asked from outside is over: so is the run.
            if let EventKind::TaskFinished { task, .. } = &ev.kind {
                if owners.get(task).map(String::as_str) == Some("lead") {
                    break;
                }
            }
            let agent = ev
                .kind
                .agent()
                .or_else(|| {
                    ev.kind
                        .task()
                        .and_then(|t| owners.get(t).map(String::as_str))
                })
                .unwrap_or("")
                .to_string();
            if matches!(
                ev.kind,
                EventKind::Agent {
                    event: HarnessEvent::TextDelta { .. }
                        | HarnessEvent::Context { .. }
                        | HarnessEvent::Step { .. }
                        | HarnessEvent::Classified { .. },
                    ..
                }
            ) {
                continue;
            }
            if let Some((line, _)) = event_line(&ev.kind) {
                println!("  {agent:>9} │ {line}");
            }
        }
    });

    let started = std::time::Instant::now();
    let done = orch
        .run(
            "lead",
            TaskSpec::new(
                "Compare the spring and summer support reviews: what improved, what did not, and what the team should do next.",
            )
            .artifacts([spring.id.to_string(), summer.id.to_string()])
            .budget(Budget::default().steps(8)),
        )
        .await?;
    let _ = printer.await;

    println!(
        "\n== result ({}, {:.1}s)\n",
        done.state.as_str(),
        started.elapsed().as_secs_f64()
    );
    match (&done.result, &done.error) {
        (Some(r), _) => println!("{r}"),
        (None, Some(e)) => println!("error: {e}"),
        _ => {}
    }

    let g = orch.graph();
    println!("\n{}", graph_report(&g).plain_text());

    println!("\n== model sessions");
    for a in &g.agents {
        match &a.session {
            Some(s) => println!(
                "  {:9} {} · vendor session {} · {} calls · {} opened · {} rotated · context {} · sent ~{} tokens, not resent ~{}",
                a.name,
                s.model,
                s.vendor.as_deref().map(|v| &v[..v.len().min(8)]).unwrap_or("-"),
                s.calls,
                s.opens,
                s.rotations,
                s.context_tokens,
                s.sent_tokens,
                s.saved_tokens
            ),
            None => println!("  {:9} {} · no managed session", a.name, a.model),
        }
    }

    for t in &g.tasks {
        for o in t.outputs.iter().filter(|o| o.kind == "diagram") {
            let bytes = ark.engine().store.read_blob(&ArtifactId(o.id.clone()))?;
            println!(
                "\n== diagram {} (by {})\n{}",
                o.name,
                t.owner,
                String::from_utf8_lossy(&bytes)
            );
        }
    }
    println!("\n== orchestration (Mermaid)\n{}", g.to_mermaid());
    let path = root.join("graph.json");
    std::fs::write(&path, serde_json::to_vec_pretty(&g)?)?;
    println!("graph: {}", path.display());
    Ok(())
}
