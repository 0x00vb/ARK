//! Named OpenAI-compatible endpoints: a router (OpenRouter, LiteLLM), a server (vLLM) or any
//! other service that speaks the OpenAI chat API. An app registers one under a name with
//! [`register`]; `name:model` then reaches it like a built-in provider (`router:gpt-5`), with
//! the endpoint's own key and models. The environment can name them too: `ARK_ENDPOINTS`
//! (`name=url,name=url`), each with its key in `<NAME>_API_KEY`.
//!
//! An endpoint is the [`OpenAi`](crate::OpenAi) provider with another base URL, key and name,
//! so it gets the same streaming, retries and usage. Its models come from its `GET /models`.

use std::collections::HashMap;
use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant};

use crate::{ProviderError, Result, PROVIDERS};

/// One OpenAI-compatible endpoint under a name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    /// The provider name in a model spec (`router` in `router:gpt-5`): lowercase letters,
    /// digits, `-` and `_`, never a built-in provider's name.
    pub name: String,
    /// What a model picker shows (`OpenRouter`).
    pub label: String,
    /// The API root, ending before `/chat/completions` (`https://openrouter.ai/api/v1`).
    pub base_url: String,
    /// The key, when the app gives it. Otherwise it is read like any API key, from
    /// [`key_var`](Endpoint::key_var): the environment first, then the app's key store.
    pub key: Option<String>,
    /// The input tokens a request to its models may have, when the app knows it
    /// ([`Provider::context_window`](crate::Provider::context_window)). Unknown: the harness
    /// keeps its smallest budget.
    pub context_window: Option<u32>,
}

impl Endpoint {
    pub fn new(name: impl Into<String>, base_url: impl Into<String>) -> Self {
        let name = name.into();
        Endpoint {
            label: name.clone(),
            name,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            key: None,
            context_window: None,
        }
    }

    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    pub fn key(mut self, key: impl Into<String>) -> Self {
        self.key = Some(key.into());
        self
    }

    /// The input tokens a request to its models may have.
    pub fn context_window(mut self, tokens: u32) -> Self {
        self.context_window = Some(tokens);
        self
    }

    /// Where its key is read from when none is given: `ROUTER_API_KEY` for `router`.
    pub fn key_var(&self) -> String {
        format!("{}_API_KEY", self.name.to_uppercase().replace('-', "_"))
    }

    /// The key to send: the given one, else the saved or environment one.
    pub fn api_key(&self) -> Option<String> {
        self.key
            .clone()
            .filter(|k| !k.trim().is_empty())
            .or_else(|| crate::keys::api_key(&self.key_var()))
    }

    fn has_key(&self) -> bool {
        self.key.as_ref().is_some_and(|k| !k.trim().is_empty())
            || crate::keys::has_api_key(&self.key_var())
    }
}

static REGISTERED: RwLock<Vec<Endpoint>> = RwLock::new(Vec::new());

fn check(e: &Endpoint) -> Result<()> {
    let refuse = |detail: String| {
        Err(ProviderError::Api {
            provider: e.name.clone(),
            detail,
        })
    };
    let valid = !e.name.is_empty()
        && e.name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if !valid {
        return refuse("an endpoint name is lowercase letters, digits, - and _".into());
    }
    if PROVIDERS.contains(&e.name.as_str()) {
        return refuse(format!(
            "`{}` is a built-in provider; give the endpoint another name",
            e.name
        ));
    }
    if !(e.base_url.starts_with("http://") || e.base_url.starts_with("https://")) {
        return refuse(format!("not an http(s) URL: {}", e.base_url));
    }
    Ok(())
}

/// Offer `endpoint` from now on, for the whole process. Registering a name again replaces it.
/// Refuses a name that is a built-in provider or not a plain lowercase word, and a URL that is
/// not http(s).
pub fn register(endpoint: Endpoint) -> Result<()> {
    check(&endpoint)?;
    let mut all = REGISTERED.write().unwrap();
    all.retain(|e| e.name != endpoint.name);
    all.push(endpoint);
    Ok(())
}

/// Stop offering the endpoint registered as `name`. Whether there was one.
pub fn unregister(name: &str) -> bool {
    let mut all = REGISTERED.write().unwrap();
    let before = all.len();
    all.retain(|e| e.name != name);
    all.len() != before
}

/// The endpoints named in `ARK_ENDPOINTS` (`name=url,name=url`). Invalid entries are skipped
/// with a warning.
fn from_env() -> Vec<Endpoint> {
    let Ok(spec) = std::env::var("ARK_ENDPOINTS") else {
        return Vec::new();
    };
    spec.split(',')
        .filter(|s| !s.trim().is_empty())
        .filter_map(|s| {
            let (name, url) = s.split_once('=')?;
            let e = Endpoint::new(name.trim(), url.trim());
            match check(&e) {
                Ok(()) => Some(e),
                Err(err) => {
                    tracing::warn!(entry = s, error = %err, "ARK_ENDPOINTS: entry skipped");
                    None
                }
            }
        })
        .collect()
}

/// Every endpoint: those an app registered, then those in the environment (a registered one
/// wins over an environment one of the same name).
pub fn endpoints() -> Vec<Endpoint> {
    let mut out = REGISTERED.read().unwrap().clone();
    for e in from_env() {
        if !out.iter().any(|o| o.name == e.name) {
            out.push(e);
        }
    }
    out
}

/// The endpoint named `name`, if there is one.
pub fn endpoint(name: &str) -> Option<Endpoint> {
    endpoints().into_iter().find(|e| e.name == name)
}

/// Whether a model spec may start with `name`: a built-in provider or an endpoint.
pub fn is_provider(name: &str) -> bool {
    PROVIDERS.contains(&name) || endpoint(name).is_some()
}

/// How long a model list is reused: long enough that opening a picker costs no request, short
/// enough that a model the endpoint adds shows up the same session.
const LISTED_FOR: Duration = Duration::from_secs(300);
const FAILED_FOR: Duration = Duration::from_secs(20);

type Listing = std::result::Result<Vec<String>, String>;
static LISTED: Mutex<Option<HashMap<String, (Instant, Listing)>>> = Mutex::new(None);

/// The endpoint's models, from its `GET /models`, reused for a few minutes. An error says why
/// it cannot be reached; it never takes longer than a few seconds.
pub async fn models(e: &Endpoint) -> Listing {
    let cache_key = format!("{}|{}", e.base_url, e.api_key().is_some());
    if let Some((at, listing)) = LISTED
        .lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .get(&cache_key)
    {
        let fresh = if listing.is_ok() {
            LISTED_FOR
        } else {
            FAILED_FOR
        };
        if at.elapsed() < fresh {
            return listing.clone();
        }
    }
    let listing = fetch_models(e).await;
    LISTED
        .lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .insert(cache_key, (Instant::now(), listing.clone()));
    listing
}

async fn fetch_models(e: &Endpoint) -> Listing {
    let mut req = reqwest::Client::new()
        .get(format!("{}/models", e.base_url))
        .timeout(Duration::from_secs(3));
    if let Some(key) = e.api_key() {
        req = req.bearer_auth(key);
    }
    let resp = req.send().await.map_err(|_| "not reachable".to_string())?;
    if !resp.status().is_success() {
        return Err(format!("models: HTTP {}", resp.status().as_u16()));
    }
    let v: serde_json::Value = resp
        .json()
        .await
        .map_err(|_| "models: not a model list".to_string())?;
    Ok(v["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| m["id"].as_str().map(str::to_string))
        .collect())
}

/// The catalog entry for an endpoint.
pub(crate) async fn backend(e: &Endpoint) -> crate::Backend {
    let has_key = e.has_key();
    let listing = models(e).await;
    let (ready, detail, models) = match (has_key, listing) {
        (false, _) => (false, format!("no key ({})", e.key_var()), Vec::new()),
        (true, Ok(models)) if models.is_empty() => (false, "no models".into(), models),
        (true, Ok(models)) => (true, "connected".into(), models),
        (true, Err(why)) => (false, why, Vec::new()),
    };
    crate::Backend {
        provider: e.name.clone(),
        label: e.label.clone(),
        kind: crate::BackendKind::Endpoint,
        ready,
        detail,
        fix: None,
        models,
        step: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_checked() {
        assert!(register(Endpoint::new("claude", "http://x/v1")).is_err());
        assert!(register(Endpoint::new("Bad Name", "http://x/v1")).is_err());
        assert!(register(Endpoint::new("t-names", "ftp://x")).is_err());
        assert!(register(Endpoint::new("t-names", "http://x/v1/")).is_ok());
        assert_eq!(endpoint("t-names").unwrap().base_url, "http://x/v1");
        assert!(is_provider("t-names"));
        assert!(unregister("t-names"));
        assert!(!is_provider("t-names"));
    }

    #[test]
    fn the_key_variable_follows_the_name() {
        assert_eq!(
            Endpoint::new("my-router", "http://x").key_var(),
            "MY_ROUTER_API_KEY"
        );
        assert_eq!(
            Endpoint::new("r", "http://x").key("k").api_key().as_deref(),
            Some("k")
        );
    }
}
