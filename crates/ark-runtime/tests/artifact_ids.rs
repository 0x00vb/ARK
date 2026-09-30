//! The model reads artifacts as short ids (`art:<12 hex>`) and hands them back that way.
//! `Engine::run_action` must resolve those to the full id before an adapter looks up the blob.

use std::sync::Arc;

use ark_core::{
    ActionDescriptor, ArtifactId, ArtifactKind, InvocationStatus, Provenance, RiskTier,
};
use ark_runtime::Engine;
use ark_tools::{ActionOutput, Adapter, Registry, RunCtx};

/// Reads the artifact it is given and reports its size.
struct Size;

#[async_trait::async_trait]
impl Adapter for Size {
    fn name(&self) -> &str {
        "size"
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![ActionDescriptor::new(
            "size.of",
            "size of an artifact",
            RiskTier::Read,
        )]
    }
    async fn run(
        &self,
        _a: &str,
        input: serde_json::Value,
        c: &RunCtx<'_>,
    ) -> ark_tools::Result<ActionOutput> {
        let id = ArtifactId(input["artifact"].as_str().unwrap_or("").to_string());
        let bytes = c.blobs.read(&id)?;
        Ok(ActionOutput::json(
            serde_json::json!({ "size": bytes.len() }),
        ))
    }
}

#[tokio::test]
async fn short_ids_and_names_reach_the_adapter_as_full_ids() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open_with(dir.path(), Registry::new().with(Arc::new(Size))).unwrap();
    let p = engine.create_project("t", "").unwrap();
    let art = engine
        .ingest_bytes(
            &p.id,
            "sample.bin",
            ArtifactKind::Binary,
            b"0123456789",
            Provenance::Ingested {
                path: "sample.bin".into(),
            },
        )
        .unwrap();
    let short = art.id.short().to_string();

    for given in [
        art.id.as_str().to_string(),
        short.clone(),
        format!("art:{short}"),
        "sample.bin".to_string(),
    ] {
        let inv = engine
            .run_action(
                &p.id,
                "size.of",
                serde_json::json!({ "artifact": given }),
                false,
            )
            .await
            .unwrap();
        assert_eq!(
            inv.status,
            InvocationStatus::Ok,
            "`{given}`: {:?}",
            inv.error
        );
        assert_eq!(inv.output["size"], 10);
        assert_eq!(
            inv.consumed,
            vec![art.id.clone()],
            "provenance records the full id"
        );
    }

    let inv = engine
        .run_action(
            &p.id,
            "size.of",
            serde_json::json!({ "artifact": "nope" }),
            false,
        )
        .await
        .unwrap();
    assert_eq!(
        inv.status,
        InvocationStatus::Failed,
        "an unknown id still fails in the adapter"
    );
}
