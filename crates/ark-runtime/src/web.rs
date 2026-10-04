//! The `web` tool ([`ark_tools::web`]) for a turn: searches go to the turn's own model first,
//! when its provider has a web search ([`Provider::web_search`]), then to the keyless engine.
//! Each agent of an orchestration gets one on its own model.

use std::sync::Arc;

use ark_model::Provider;
use ark_tools::web::{Hit, SearchBackend, WebAdapter, FETCH, SEARCH};
use ark_tools::AdapterError;

use crate::distill::Distiller;

/// Both web actions.
pub const ACTIONS: [&str; 2] = [SEARCH, FETCH];

/// A provider's own web search as a [`SearchBackend`].
pub struct ModelSearch(pub Arc<dyn Provider>);

#[async_trait::async_trait]
impl SearchBackend for ModelSearch {
    fn name(&self) -> String {
        self.0.describe()
    }

    async fn search(&self, query: &str, limit: usize) -> ark_tools::Result<Vec<Hit>> {
        match self.0.web_search(query, limit).await {
            None => Err(AdapterError::Tool("this model has no web search".into())),
            Some(Err(e)) => Err(AdapterError::Tool(e.to_string())),
            Some(Ok(hits)) => Ok(hits
                .into_iter()
                .map(|h| Hit {
                    title: h.title,
                    url: h.url,
                    snippet: h.snippet,
                })
                .collect()),
        }
    }
}

/// `base` with `model`'s own search asked first.
pub fn on_model(base: &WebAdapter, model: &Arc<dyn Provider>) -> WebAdapter {
    base.clone()
        .search_with(Arc::new(ModelSearch(model.clone())))
}

/// The evidence cards of web results ([`ark_tools::web::distill`]).
pub fn distiller() -> Distiller {
    Arc::new(ark_tools::web::distill)
}

/// The web as a per-agent tool: each owner searches on its own model.
pub struct Web(pub WebAdapter);

impl crate::per_agent::PerAgent for Web {
    fn listing(&self) -> Arc<dyn ark_tools::Adapter> {
        Arc::new(self.0.clone())
    }
    fn for_owner(&self, owner: &crate::per_agent::Owner) -> Arc<dyn ark_tools::Adapter> {
        Arc::new(on_model(&self.0, owner.model))
    }
    fn distiller(&self) -> Option<Distiller> {
        Some(distiller())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_model::{Completion, Message, ToolDef, WebHit};
    use ark_tools::{Adapter, BlobResolver, RunCtx};

    struct Searching;
    #[async_trait::async_trait]
    impl Provider for Searching {
        async fn complete(
            &self,
            _s: &str,
            _m: &[Message],
            _t: &[ToolDef],
        ) -> ark_model::Result<Completion> {
            Ok(Completion::default())
        }
        async fn web_search(&self, q: &str, _l: usize) -> Option<ark_model::Result<Vec<WebHit>>> {
            Some(Ok(vec![WebHit {
                title: format!("About {q}"),
                url: "https://example.org/a".into(),
                snippet: String::new(),
            }]))
        }
        fn describe(&self) -> String {
            "searching model".into()
        }
    }

    struct Plain;
    #[async_trait::async_trait]
    impl Provider for Plain {
        async fn complete(
            &self,
            _s: &str,
            _m: &[Message],
            _t: &[ToolDef],
        ) -> ark_model::Result<Completion> {
            Ok(Completion::default())
        }
        fn describe(&self) -> String {
            "plain model".into()
        }
    }

    struct NoBlobs;
    impl BlobResolver for NoBlobs {
        fn path(&self, _id: &ark_core::ArtifactId) -> Option<std::path::PathBuf> {
            None
        }
        fn read(&self, id: &ark_core::ArtifactId) -> ark_tools::Result<Vec<u8>> {
            Err(AdapterError::ArtifactNotFound(id.to_string()))
        }
    }

    #[tokio::test]
    async fn the_models_own_search_goes_first() {
        let base = WebAdapter::new().backends(vec![]);
        let model: Arc<dyn Provider> = Arc::new(Searching);
        let out = on_model(&base, &model)
            .run(
                SEARCH,
                serde_json::json!({ "query": "kettles" }),
                &RunCtx::new(&NoBlobs),
            )
            .await
            .ok()
            .unwrap()
            .output;
        assert_eq!(out["engine"], "searching model");
        assert_eq!(out["results"][0]["title"], "About kettles");
    }

    #[tokio::test]
    async fn a_model_without_search_says_so_and_the_next_engine_is_asked() {
        let base = WebAdapter::new().backends(vec![]);
        let model: Arc<dyn Provider> = Arc::new(Plain);
        let out = on_model(&base, &model)
            .run(
                SEARCH,
                serde_json::json!({ "query": "kettles" }),
                &RunCtx::new(&NoBlobs),
            )
            .await
            .ok()
            .unwrap()
            .output;
        assert_eq!(
            out["note"],
            "no engine returned results (plain model: tool error: this model has no web search)"
        );
    }
}
