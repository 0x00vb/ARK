use ark_bench::{
    cassette::Cassette,
    faults::{FaultPlan, Faulty},
    record::{Metered, Recorder},
};
use ark_sdk::prelude::ark_model::{Completion, Message, Provider, Result, ToolDef, Usage};
use async_trait::async_trait;
use std::sync::Arc;

struct Bound(String);
#[async_trait]
impl Provider for Bound {
    fn for_conversation(&self, scope: &str) -> Result<Option<Arc<dyn Provider>>> {
        Ok(Some(Arc::new(Self(scope.into()))))
    }
    fn describe(&self) -> String {
        "scoped:model".into()
    }
    async fn complete(&self, _: &str, _: &[Message], _: &[ToolDef]) -> Result<Completion> {
        Ok(Completion {
            content: self.0.clone(),
            usage: Usage::measured(4, 1),
            ..Default::default()
        })
    }
}

#[tokio::test]
async fn scoped_views_preserve_benchmark_accounting_and_cassette_sequence() {
    let recorder = Recorder::new();
    let cassette = Cassette::record();
    let base: Arc<dyn Provider> = Arc::new(Bound("unbound".into()));
    let taped = cassette.provider("scope", 0, Some(base));
    let metered = Metered::wrap(taped, recorder.clone());
    let wrapped = Faulty::wrap(metered, &FaultPlan::default(), 0, recorder.clone());
    for scope in ["chat/A", "chat/B"] {
        let bound = wrapped.for_conversation(scope).unwrap().unwrap();
        assert_eq!(
            bound.complete("code", &[], &[]).await.unwrap().content,
            scope
        );
    }
    assert_eq!(recorder.len(), 2);
    assert_eq!(
        recorder
            .records()
            .iter()
            .map(|r| r.usage.total())
            .sum::<u32>(),
        10
    );
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("record.jsonl");
    cassette.save(&path).unwrap();
    let entries: Vec<serde_json::Value> = std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        entries
            .iter()
            .map(|v| v["seq"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        [0, 1]
    );
}
