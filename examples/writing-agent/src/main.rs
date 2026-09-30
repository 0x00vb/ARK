//! # writing-agent
//!
//! A novelist's assistant, built on the *same* ARK runtime as the security workstation it was
//! extracted from — with zero changes to any ARK crate. This is the concrete answer to the
//! test ARK holds itself to:
//!
//! > Could this exact harness power an agent whose only job is helping an author write a novel?
//!
//! It registers manuscript tools, a writer's persona, writing-domain capabilities, and a
//! custom result distiller. The runtime's context budget, model routing, evidence model,
//! working-state graph, approval gate, and reproducible notebook all apply unchanged.
//!
//! ```text
//! ARK_PROVIDER=anthropic ARK_MODEL=claude-sonnet-5-5 \
//!   cargo run -p writing-agent -- "is chapter 1 too long, and does it mention the locket?"
//! ```

use std::sync::Arc;

use ark_sdk::prelude::*;

// ---- manuscript tools ------------------------------------------------------

/// Read/inspect chapters that live as files in the project workspace.
struct Manuscript;

#[async_trait::async_trait]
impl Adapter for Manuscript {
    fn name(&self) -> &str {
        "manuscript"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![
            ActionDescriptor::new("manuscript.list", "list the chapter files in the manuscript", RiskTier::Read)
                .with_capabilities([Capability::new("structure-analysis")]),
            ActionDescriptor::new("manuscript.read", "read a chapter's text", RiskTier::Read)
                .with_capabilities([Capability::new("revision")])
                .with_input_schema(file_schema("the chapter filename, e.g. chapter1.md")),
            ActionDescriptor::new("manuscript.wordcount", "count words and paragraphs in a chapter", RiskTier::Read)
                .with_capabilities([Capability::new("structure-analysis")])
                .with_input_schema(file_schema("the chapter filename")),
            ActionDescriptor::new("manuscript.save", "write text to a chapter (creates or overwrites)", RiskTier::Mutate)
                .with_capabilities([Capability::new("drafting")])
                .with_input_schema(serde_json::json!({
                    "type": "object", "required": ["file", "text"],
                    "properties": {
                        "file": { "type": "string" },
                        "text": { "type": "string" }
                    }
                })),
        ]
    }
    async fn run(&self, action: &str, input: serde_json::Value, ctx: &RunCtx<'_>) -> ark_tools::Result<ActionOutput> {
        let ws = ctx.workspace.clone().unwrap_or_else(|| std::path::PathBuf::from("."));
        match action {
            "manuscript.list" => {
                let mut entries: Vec<String> = std::fs::read_dir(&ws)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .filter_map(|e| e.file_name().into_string().ok())
                    .filter(|n| n.ends_with(".md") || n.ends_with(".txt"))
                    .collect();
                entries.sort();
                Ok(ActionOutput::json(serde_json::json!({ "path": ".", "entries": entries })))
            }
            "manuscript.read" => {
                let file = req_str(&input, "file")?;
                let text = read_chapter(&ws, &file)?;
                let lines = text.lines().count();
                let excerpt: String = text.chars().take(600).collect();
                Ok(ActionOutput::json(serde_json::json!({
                    "path": file, "lines": lines, "words": word_count(&text), "excerpt": excerpt
                })))
            }
            "manuscript.wordcount" => {
                let file = req_str(&input, "file")?;
                let text = read_chapter(&ws, &file)?;
                let words = word_count(&text);
                let paras = text.split("\n\n").filter(|p| !p.trim().is_empty()).count();
                Ok(ActionOutput::json(serde_json::json!({
                    "path": file, "words": words, "paragraphs": paras, "lines": text.lines().count()
                })))
            }
            "manuscript.save" => {
                let file = req_str(&input, "file")?;
                let text = req_str(&input, "text")?;
                let path = ws.join(&file);
                let added = text.lines().count() as u64;
                std::fs::write(&path, text.as_bytes())
                    .map_err(|e| ark_tools::AdapterError::Tool(format!("write {file}: {e}")))?;
                Ok(ActionOutput::json(serde_json::json!({ "path": file, "added": added }))
                    .with_command(vec!["write".into(), file]))
            }
            other => Err(ark_tools::AdapterError::UnknownAction(other.into())),
        }
    }
}

/// Search the whole manuscript for a term — continuity checks (a character, a prop, a place).
struct Continuity;

#[async_trait::async_trait]
impl Adapter for Continuity {
    fn name(&self) -> &str {
        "continuity"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![ActionDescriptor::new("continuity.search", "find every mention of a term across all chapters", RiskTier::Read)
            .with_capabilities([Capability::new("continuity-check")])
            .with_input_schema(serde_json::json!({
                "type": "object", "required": ["term"],
                "properties": { "term": { "type": "string", "description": "a character name, place, or object" } }
            }))]
    }
    async fn run(&self, _action: &str, input: serde_json::Value, ctx: &RunCtx<'_>) -> ark_tools::Result<ActionOutput> {
        let ws = ctx.workspace.clone().unwrap_or_else(|| std::path::PathBuf::from("."));
        let term = req_str(&input, "term")?.to_lowercase();
        let mut matches = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&ws) {
            let mut files: Vec<_> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().map(|e| e == "md" || e == "txt").unwrap_or(false)).collect();
            files.sort();
            for path in files {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
                if let Ok(text) = std::fs::read_to_string(&path) {
                    for (i, line) in text.lines().enumerate() {
                        if line.to_lowercase().contains(&term) {
                            matches.push(serde_json::json!({ "path": name, "line": i + 1 }));
                        }
                    }
                }
            }
        }
        Ok(ActionOutput::json(serde_json::json!({ "query": term, "count": matches.len(), "matches": matches })))
    }
}

// ---- a domain distiller ----------------------------------------------------

/// Turn manuscript tool outputs into tight, writer-facing evidence summaries. Everything the
/// distiller doesn't recognize falls through to ARK's common distiller and generic fallback.
fn writing_distiller() -> DistillerSet {
    let d: Distiller = Arc::new(|action: &str, out: &serde_json::Value| {
        let n = |k: &str| out.get(k).and_then(|v| v.as_u64());
        let s = |k: &str| out.get(k).and_then(|v| v.as_str()).map(str::to_string);
        match action {
            "manuscript.wordcount" => {
                let words = n("words").unwrap_or(0);
                let paras = n("paragraphs").unwrap_or(0);
                let file = s("path").unwrap_or_default();
                Some((format!("{file}: {words} words in {paras} paragraph(s)"), vec![format!("words={words}")]))
            }
            "manuscript.read" => {
                let file = s("path").unwrap_or_default();
                let words = n("words").unwrap_or(0);
                Some((format!("read {file} ({words} words)"), vec![file]))
            }
            _ => None,
        }
    });
    DistillerSet::with_common().with(d)
}

// ---- wiring ----------------------------------------------------------------

fn build_ark(store_root: impl Into<std::path::PathBuf>, provider: Arc<dyn ark_model::Provider>) -> Result<Ark, ArkError> {
    let profile = AgentProfile::builder()
        .instructions(
            "You are a sharp, encouraging developmental editor helping an author with their novel. You \
             work from the manuscript itself, never from assumptions: read chapters, count their length, \
             and search for how characters and objects recur before you judge pacing or continuity. Be \
             concrete and cite the chapter and line. Distinguish what the text actually says (a fact) from \
             your editorial opinion (an inference). Respect the author's voice — suggest, don't rewrite, \
             unless asked.",
        )
        .capabilities(CapabilityCatalog::new([
            CapabilitySpec::new("structure-analysis", "measure a chapter's length and shape").with_synonyms(["pacing", "length", "wordcount"]),
            CapabilitySpec::new("continuity-check", "track how a character, place or object recurs").with_synonyms(["continuity", "consistency"]),
            CapabilitySpec::new("revision", "read and assess a passage for revision").with_synonyms(["edit", "review", "read"]),
            CapabilitySpec::new("drafting", "write or save new prose").with_synonyms(["write", "compose", "draft"]),
        ]))
        // A domain marker so "does chapter 1 foreshadow …?" is treated as an investigation.
        .investigation_markers(["why", "how", "compare", "foreshadow", "continuity", "consistent", "pacing", " and "])
        .distillers(writing_distiller())
        .core_actions(["manuscript.list", "manuscript.read", "manuscript.wordcount", "continuity.search"])
        .build();

    Ark::builder()
        .store_root(store_root)
        .tool(Arc::new(Manuscript))
        .tool(Arc::new(Continuity))
        .profile(profile)
        .provider(provider)
        .build()
}

/// Seed a tiny sample manuscript in the project workspace so the example runs out of the box.
fn seed_manuscript(ark: &Ark, project: &ProjectId) -> std::io::Result<()> {
    let ws = ark.engine().workspace_dir(project).expect("workspace dir");
    std::fs::write(
        ws.join("chapter1.md"),
        "# Chapter 1\n\nMara turned the brass locket over in her palm, its hinge stiff with age. \
         The train pulled out of Varga station as the last light failed.\n\nShe had promised her \
         grandmother she would never open it. She was already regretting the promise.\n",
    )?;
    std::fs::write(
        ws.join("chapter2.md"),
        "# Chapter 2\n\nThe locket stayed cold against her collarbone all the way to the coast. \
         Varga was three days behind her now.\n",
    )?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let task = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
    let task = if task.is_empty() {
        "How long is chapter 1, and does the locket appear again later?".to_string()
    } else {
        task
    };

    let provider = ark_model::from_env_arc()?;
    let ark = build_ark("./writing-agent-workspace", provider)?;
    let project = ark.create_project("the-locket", "a debut novel")?;
    seed_manuscript(&ark, &project.id)?;

    let outcome = ark
        .ask_with(&project.id, &task, |ev| match ev {
            HarnessEvent::ToolResult { action, summary, .. } => eprintln!("  · {action}: {summary}"),
            HarnessEvent::Plan { steps } => eprintln!("  plan: {}", steps.join(" → ")),
            _ => {}
        })
        .await?;

    println!("\n{}", outcome.answer);
    println!("\n[{} tokens, {} tool call(s), {} evidence card(s)]",
        outcome.metrics.total_tokens(), outcome.metrics.tool_calls, outcome.metrics.cards_created);
    Ok(())
}

// ---- helpers ---------------------------------------------------------------

fn file_schema(desc: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "object", "required": ["file"],
        "properties": { "file": { "type": "string", "description": desc } }
    })
}

fn req_str(input: &serde_json::Value, key: &str) -> ark_tools::Result<String> {
    input
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| ark_tools::AdapterError::bad_input("manuscript", format!("missing string field `{key}`")))
}

fn read_chapter(ws: &std::path::Path, file: &str) -> ark_tools::Result<String> {
    // Confine to the workspace: no path traversal.
    if file.contains("..") || file.starts_with('/') {
        return Err(ark_tools::AdapterError::bad_input("manuscript", "file must be a plain name inside the workspace"));
    }
    std::fs::read_to_string(ws.join(file))
        .map_err(|e| ark_tools::AdapterError::Tool(format!("read {file}: {e}")))
}

fn word_count(text: &str) -> u64 {
    text.split_whitespace().filter(|w| w.chars().any(|c| c.is_alphanumeric())).count() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_model::{Completion, MockProvider, ToolCall, Usage};

    #[tokio::test]
    async fn editor_agent_counts_a_chapter_and_checks_continuity() {
        let dir = tempfile::tempdir().unwrap();
        // Script: wordcount chapter1, search for "locket", then answer.
        let c1 = ToolCall { id: "c1".into(), name: "manuscript.wordcount".into(), arguments: serde_json::json!({ "file": "chapter1.md" }) };
        let c2 = ToolCall { id: "c2".into(), name: "continuity.search".into(), arguments: serde_json::json!({ "term": "locket" }) };
        let script = vec![
            // The planner runs first (the task has an " and " marker → an investigation).
            Completion { content: "measure chapter 1 length\ncheck whether the locket recurs".into(), tool_calls: vec![], usage: Usage::measured(40, 10) },
            Completion { content: String::new(), tool_calls: vec![c1], usage: Usage::measured(200, 12) },
            Completion { content: String::new(), tool_calls: vec![c2], usage: Usage::measured(220, 12) },
            Completion { content: "Chapter 1 runs about 60 words [inv:1]; the locket recurs in chapter 2 [inv:2], so continuity holds.".into(), tool_calls: vec![], usage: Usage::measured(240, 30) },
        ];
        let provider: Arc<dyn ark_model::Provider> = Arc::new(MockProvider::new("mock", script));

        let ark = build_ark(dir.path(), provider).unwrap();
        let project = ark.create_project("the-locket", "a debut novel").unwrap();
        seed_manuscript(&ark, &project.id).unwrap();

        let outcome = ark.ask(&project.id, "how long is chapter 1 and does the locket recur?").await.unwrap();

        assert_eq!(outcome.metrics.tool_calls, 2, "both manuscript tools ran");
        assert!(outcome.answer.contains("locket"), "answer: {}", outcome.answer);
        // The custom distiller produced a writer-facing wordcount summary.
        let cards = ark.engine().store.list_evidence_cards(&project.id).unwrap();
        assert!(cards.iter().any(|c| c.summary.contains("words in")), "distiller summaries: {:?}", cards.iter().map(|c| &c.summary).collect::<Vec<_>>());
    }

    #[test]
    fn word_count_ignores_whitespace_noise() {
        assert_eq!(word_count("one two   three\n\nfour"), 4);
    }

    #[tokio::test]
    async fn save_is_gated_behind_approval() {
        let dir = tempfile::tempdir().unwrap();
        let save = ToolCall { id: "c1".into(), name: "manuscript.save".into(), arguments: serde_json::json!({ "file": "new.md", "text": "hello" }) };
        let script = vec![
            Completion { content: String::new(), tool_calls: vec![save], usage: Usage::measured(80, 8) },
            Completion { content: "I need approval to write that file [inv].".into(), tool_calls: vec![], usage: Usage::measured(90, 9) },
        ];
        let provider: Arc<dyn ark_model::Provider> = Arc::new(MockProvider::new("mock", script));
        let ark = build_ark(dir.path(), provider).unwrap(); // allow_mutate defaults to false
        let project = ark.create_project("t", "").unwrap();
        seed_manuscript(&ark, &project.id).unwrap();
        let _ = ark.ask(&project.id, "save a new chapter").await.unwrap();
        // The mutate action must NOT have written the file (it was denied).
        let ws = ark.engine().workspace_dir(&project.id).unwrap();
        assert!(!ws.join("new.md").exists(), "save must be gated behind human approval");
    }
}
