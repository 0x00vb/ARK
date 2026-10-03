//! Diagrams as a tool. Register [`Diagrams`] and an agent can store a validated Mermaid
//! diagram as an artifact (`diagram.create`), revise one (pass `revises`), and read one back
//! (`diagram.read`), for example to review a diagram another agent produced. Validation and
//! the stored format are in `model` ([`Diagram`], [`validate`]); nothing here renders. They
//! live here, not in `ark-core`, because the core names no file format.

mod model;
use ark_core::{ActionDescriptor, ArtifactKind, RiskTier};
use async_trait::async_trait;
pub use model::*;
use serde_json::{json, Value};

use crate::{arg_artifact, ActionOutput, Adapter, AdapterError, ProducedArtifact, Result, RunCtx};

pub const CREATE: &str = "diagram.create";
pub const READ: &str = "diagram.read";

/// The diagram tool. Opt-in: register it on the agents that should draw.
pub struct Diagrams;

#[async_trait]
impl Adapter for Diagrams {
    fn name(&self) -> &str {
        "diagram"
    }

    fn actions(&self) -> Vec<ActionDescriptor> {
        let kinds: Vec<&str> = DiagramKind::ALL.iter().map(|k| k.as_str()).collect();
        vec![
            ActionDescriptor::new(
                CREATE,
                "store a diagram (Mermaid source) as an artifact after checking its structure; \
                 sequence, class, state and er kinds use those Mermaid types, the others a flowchart",
                RiskTier::Read,
            )
            .reusable()
            .with_input_schema(json!({
                "type": "object",
                "required": ["kind", "title", "source"],
                "properties": {
                    "kind": { "type": "string", "enum": kinds },
                    "title": { "type": "string" },
                    "source": { "type": "string", "description": "Mermaid text, starting with its type line (flowchart LR, sequenceDiagram, classDiagram, stateDiagram-v2, erDiagram)" },
                    "revises": { "type": "string", "description": "art:<id> of the diagram this one replaces" }
                }
            })),
            ActionDescriptor::new(READ, "read a stored diagram's source and metadata", RiskTier::Read)
                .reusable()
                .with_input_schema(crate::artifact_input_schema("art:<id> of the diagram")),
        ]
    }

    async fn run(&self, action: &str, input: Value, ctx: &RunCtx<'_>) -> Result<ActionOutput> {
        match action {
            CREATE => create(input, ctx),
            READ => {
                let id = arg_artifact(action, &input)?;
                let d = load(ctx, &id)?;
                Ok(ActionOutput::json(json!({
                    "artifact": id.as_str(),
                    "kind": d.kind,
                    "title": d.title,
                    "version": d.version,
                    "revises": d.revises,
                    "source": d.source,
                })))
            }
            other => Err(AdapterError::UnknownAction(other.to_string())),
        }
    }
}

fn load(ctx: &RunCtx<'_>, id: &ark_core::ArtifactId) -> Result<Diagram> {
    let bytes = ctx.blobs.read(id)?;
    let text = String::from_utf8(bytes)
        .map_err(|_| AdapterError::bad_input(READ, "the artifact is not text"))?;
    Diagram::from_text(&text)
        .ok_or_else(|| AdapterError::bad_input(READ, "the artifact is not a diagram"))
}

fn create(input: Value, ctx: &RunCtx<'_>) -> Result<ActionOutput> {
    let s = |k: &str| input.get(k).and_then(Value::as_str).unwrap_or("").trim();
    let kind = DiagramKind::parse(s("kind"))
        .ok_or_else(|| AdapterError::bad_input(CREATE, format!("unknown kind `{}`", s("kind"))))?;
    let title = s("title");
    if title.is_empty() {
        return Err(AdapterError::bad_input(CREATE, "a diagram needs a title"));
    }
    let mut d = Diagram::new(kind, title, s("source"));
    let revises = s("revises").trim_start_matches("art:");
    if !revises.is_empty() {
        let id = ark_core::ArtifactId(revises.to_string());
        let prev = load(ctx, &id)?;
        d.version = prev.version + 1;
        d.revises = Some(revises.to_string());
    }
    if let Err(issues) = d.validate() {
        let list: Vec<String> = issues.iter().map(|i| i.to_string()).collect();
        return Err(AdapterError::bad_input(
            CREATE,
            format!(
                "the diagram has problems; fix them and call again: {}",
                list.join("; ")
            ),
        ));
    }
    let file = format!("{}.mmd", slug(title));
    Ok(ActionOutput::json(json!({
        "stored": file,
        "kind": d.kind,
        "title": d.title,
        "version": d.version,
        "revises": d.revises,
        "lines": d.source.lines().count(),
    }))
    .with_artifact(ProducedArtifact::bytes(
        ArtifactKind::named(DIAGRAM_KIND),
        file,
        d.to_text().into_bytes(),
    )))
}

fn slug(title: &str) -> String {
    let s: String = title
        .chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let s = s
        .split('-')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if s.is_empty() {
        "diagram".into()
    } else {
        s.chars().take(60).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BlobResolver;
    use ark_core::ArtifactId;
    use std::collections::HashMap;
    use std::path::PathBuf;

    struct Mem(HashMap<String, Vec<u8>>);
    impl BlobResolver for Mem {
        fn path(&self, _id: &ArtifactId) -> Option<PathBuf> {
            None
        }
        fn read(&self, id: &ArtifactId) -> Result<Vec<u8>> {
            self.0
                .get(id.as_str())
                .cloned()
                .ok_or_else(|| AdapterError::ArtifactNotFound(id.to_string()))
        }
    }

    #[tokio::test]
    async fn creates_validates_and_revises() {
        let blobs = Mem(HashMap::new());
        let ctx = RunCtx::new(&blobs);
        let out = Diagrams
            .run(
                CREATE,
                json!({"kind": "sequence", "title": "Delegation flow", "source": "sequenceDiagram\n C->>W: task\n W-->>C: result"}),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(out.output["version"], 1);
        assert_eq!(out.produced.len(), 1);
        assert_eq!(out.produced[0].name, "delegation-flow.mmd");
        assert_eq!(out.produced[0].kind.as_tag(), "diagram");
        let text = String::from_utf8(out.produced[0].bytes.clone()).unwrap();

        let blobs = Mem(HashMap::from([("abc".to_string(), text.into_bytes())]));
        let ctx = RunCtx::new(&blobs);
        let read = Diagrams
            .run(READ, json!({"artifact": "abc"}), &ctx)
            .await
            .unwrap();
        assert_eq!(read.output["title"], "Delegation flow");
        let rev = Diagrams
            .run(
                CREATE,
                json!({"kind": "sequence", "title": "Delegation flow", "source": "sequenceDiagram\n C->>W: task", "revises": "art:abc"}),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(rev.output["version"], 2);
        assert_eq!(rev.output["revises"], "abc");
    }

    #[tokio::test]
    async fn an_invalid_diagram_is_refused_with_its_problems() {
        let blobs = Mem(HashMap::new());
        let ctx = RunCtx::new(&blobs);
        let Err(e) = Diagrams
            .run(
                CREATE,
                json!({"kind": "class", "title": "x", "source": "flowchart LR\n A --> B"}),
                &ctx,
            )
            .await
        else {
            panic!("an invalid diagram was stored");
        };
        assert!(e.to_string().contains("classDiagram"), "{e}");
    }
}
