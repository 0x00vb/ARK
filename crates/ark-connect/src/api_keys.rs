//! API keys for the hosted model APIs, kept in the same secret store as account tokens (the OS
//! keychain), so apps stop keeping their own. Which providers have a saved key is listed in
//! `<root>/api-keys.json` (names only), so the model list can say a key is there without reading
//! the keychain. [`ApiKeys`] is ark-model's [`KeySource`]: a key saved now is used by the next
//! request, and a key in the environment still comes first.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ark_model::keys::KeySource;

use crate::{ConnectError, Secrets};

/// The providers a key can be saved for: (provider, name, environment variable).
pub const PROVIDERS: &[(&str, &str, &str)] = &[
    ("anthropic", "Anthropic", "ANTHROPIC_API_KEY"),
    ("openai", "OpenAI", "OPENAI_API_KEY"),
];

const INDEX: &str = "api-keys.json";

fn provider_of_var(var: &str) -> Option<&'static str> {
    PROVIDERS.iter().find(|p| p.2 == var).map(|p| p.0)
}

fn known(provider: &str) -> Result<(), ConnectError> {
    if PROVIDERS.iter().any(|p| p.0 == provider) {
        Ok(())
    } else {
        Err(ConnectError::Unknown(provider.to_string()))
    }
}

/// Saved API keys. Cheap to clone; clones share the cache.
#[derive(Clone)]
pub struct ApiKeys {
    secrets: Arc<dyn Secrets>,
    index: PathBuf,
    cache: Arc<Mutex<HashMap<String, String>>>,
    /// Where keys are checked (tests point it at a local server).
    bases: HashMap<String, String>,
}

impl ApiKeys {
    pub fn open(root: &Path, secrets: Arc<dyn Secrets>) -> Self {
        ApiKeys {
            secrets,
            index: root.join(INDEX),
            cache: Arc::default(),
            bases: HashMap::new(),
        }
    }

    /// Check `provider`'s keys against `base` instead (tests).
    pub fn with_base(mut self, provider: &str, base: &str) -> Self {
        self.bases
            .insert(provider.into(), base.trim_end_matches('/').into());
        self
    }

    /// The providers with a saved key.
    pub fn saved(&self) -> Vec<String> {
        std::fs::read_to_string(&self.index)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    fn write_index(&self, list: &[String]) -> Result<(), ConnectError> {
        let tmp = self.index.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(list).unwrap_or_default())?;
        std::fs::rename(&tmp, &self.index)?;
        Ok(())
    }

    /// Check `key` with the provider, then keep it. A key the provider refuses is not kept.
    pub async fn save(&self, provider: &str, key: &str) -> Result<(), ConnectError> {
        known(provider)?;
        let key = key.trim();
        if key.is_empty() {
            return Err(ConnectError::Provider("Paste the key first.".into()));
        }
        self.check(provider, key).await?;
        self.secrets.set(&secret(provider), key)?;
        self.cache
            .lock()
            .unwrap()
            .insert(provider.to_string(), key.to_string());
        let mut list = self.saved();
        if !list.iter().any(|p| p == provider) {
            list.push(provider.to_string());
            self.write_index(&list)?;
        }
        Ok(())
    }

    /// Remove a saved key.
    pub fn forget(&self, provider: &str) -> Result<(), ConnectError> {
        known(provider)?;
        self.secrets.delete(&secret(provider))?;
        self.cache.lock().unwrap().remove(provider);
        let list: Vec<String> = self.saved().into_iter().filter(|p| p != provider).collect();
        self.write_index(&list)
    }

    /// Whether the provider takes `key`: one cheap request that lists its models.
    pub async fn check(&self, provider: &str, key: &str) -> Result<(), ConnectError> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()?;
        // Checked where the model calls go: a base the app set, the provider's base URL
        // variable (as ark-model reads it), or the provider's own API.
        let models = |var: &str, default: &str| match self.bases.get(provider) {
            Some(b) => format!("{b}/v1/models"),
            None => match std::env::var(var) {
                Ok(b) if !b.trim().is_empty() => {
                    format!("{}/models", b.trim().trim_end_matches('/'))
                }
                _ => format!("{default}/models"),
            },
        };
        let req = match provider {
            "anthropic" => client
                .get(models("ANTHROPIC_BASE_URL", "https://api.anthropic.com/v1"))
                .header("x-api-key", key)
                .header("anthropic-version", "2023-06-01"),
            _ => client
                .get(models("OPENAI_BASE_URL", "https://api.openai.com/v1"))
                .bearer_auth(key),
        };
        let res = req.send().await.map_err(|_| {
            ConnectError::Provider("Could not reach the provider. Check the connection.".into())
        })?;
        match res.status().as_u16() {
            200..=299 => Ok(()),
            401 | 403 => Err(ConnectError::Provider("That key was not accepted.".into())),
            s => Err(ConnectError::Provider(format!(
                "The provider answered {s}. Try again in a moment."
            ))),
        }
    }
}

fn secret(provider: &str) -> String {
    format!("api-key:{provider}")
}

impl KeySource for ApiKeys {
    fn key(&self, var: &str) -> Option<String> {
        let provider = provider_of_var(var)?;
        if let Some(k) = self.cache.lock().unwrap().get(provider) {
            return Some(k.clone());
        }
        if !self.has(var) {
            return None;
        }
        let k = self.secrets.get(&secret(provider)).ok().flatten()?;
        self.cache
            .lock()
            .unwrap()
            .insert(provider.to_string(), k.clone());
        Some(k)
    }

    fn has(&self, var: &str) -> bool {
        provider_of_var(var).is_some_and(|p| self.saved().iter().any(|s| s == p))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemorySecrets;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A provider on loopback that takes `good` and refuses anything else.
    async fn provider(good: &'static str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut s, _)) = listener.accept().await else {
                    break;
                };
                let mut buf = vec![0u8; 4096];
                let n = s.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_lowercase();
                let ok = req.contains(&format!("x-api-key: {good}"));
                let status = if ok { "200 OK" } else { "401 Unauthorized" };
                let _ = s
                    .write_all(
                        format!("HTTP/1.1 {status}\r\ncontent-length: 2\r\n\r\n{{}}").as_bytes(),
                    )
                    .await;
            }
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn a_key_is_checked_kept_found_by_the_model_layer_and_forgotten() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = Arc::new(MemorySecrets::default());
        let base = provider("sk-good").await;
        let keys = ApiKeys::open(dir.path(), secrets.clone()).with_base("anthropic", &base);
        let refused = keys.save("anthropic", "sk-bad").await.unwrap_err();
        assert!(
            refused.to_string().contains("That key was not accepted."),
            "{refused}"
        );
        assert!(keys.saved().is_empty() && !keys.has("ANTHROPIC_API_KEY"));
        keys.save("anthropic", " sk-good ").await.unwrap();
        assert_eq!(keys.saved(), ["anthropic"]);
        // Only the provider's name is in the store; the key is in the secret store.
        let index = std::fs::read_to_string(dir.path().join(INDEX)).unwrap();
        assert!(!index.contains("sk-good"));
        assert_eq!(
            secrets.get("api-key:anthropic").unwrap().as_deref(),
            Some("sk-good")
        );
        // A fresh handle (the next launch) reads it from the secret store.
        let again = ApiKeys::open(dir.path(), secrets.clone());
        assert_eq!(again.key("ANTHROPIC_API_KEY").as_deref(), Some("sk-good"));
        assert_eq!(again.key("OPENAI_API_KEY"), None);
        assert!(keys.save("mistral", "x").await.is_err());
        again.forget("anthropic").unwrap();
        assert!(again.saved().is_empty());
        assert_eq!(secrets.get("api-key:anthropic").unwrap(), None);
        assert_eq!(
            ApiKeys::open(dir.path(), secrets).key("ANTHROPIC_API_KEY"),
            None
        );
    }
}
