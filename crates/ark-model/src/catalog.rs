//! What models this machine can use right now: signed-in subscriptions, local Ollama models,
//! APIs with a key, and the named endpoints an app registered ([`crate::endpoints`]).
//! Front-ends build their model picker from it.

use crate::cli::{self, Cli};
use serde::Serialize;
use std::time::Duration;

/// The built-in provider names a model spec may start with (`claude:sonnet`, `ollama qwen3:4b`).
/// Named endpoints add theirs ([`crate::endpoints::is_provider`]).
pub const PROVIDERS: &[&str] = &[
    "ollama",
    "openai",
    "anthropic",
    "claude",
    "codex",
    "opencode-go",
    "mock",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BackendKind {
    Subscription,
    Local,
    Api,
    /// A named OpenAI-compatible endpoint ([`crate::endpoints`]).
    Endpoint,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CatalogState {
    Current,
    Unavailable,
}

/// A model the provider advertised, with the effort choices it accepts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ModelOption {
    /// The identifier used in a model spec.
    pub id: String,
    /// A short name for a picker.
    pub label: String,
    /// Supported effort values, in the provider's preferred order.
    pub efforts: Vec<String>,
    /// The effort used when a spec does not name one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_effort: Option<String>,
}

/// One way to reach models.
#[derive(Clone, Debug, Serialize)]
pub struct Backend {
    /// The provider name in a model spec.
    pub provider: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
    pub kind: BackendKind,
    /// Configured and selectable. This does not prove current inference entitlement.
    pub ready: bool,
    /// How it is connected, or why it is not.
    pub detail: String,
    /// A command that makes it ready (sign in, install), when it is not.
    pub fix: Option<String>,
    /// Models to offer. Others can still be typed as a spec.
    pub models: Vec<String>,
    /// Rich picker data when the provider exposes it. `models` remains for old clients.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub model_options: Vec<ModelOption>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub catalog_state: Option<CatalogState>,
    /// A subscription's program not ready: the step it needs, `install` or `sign_in`, which a
    /// window can run on the computer it lists (ADR 0052).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<&'static str>,
}

/// Check every backend. Runs the checks at once; a slow or missing one costs at most a few
/// seconds and never fails the listing.
pub async fn catalog() -> Vec<Backend> {
    let named = crate::endpoints::endpoints();
    let (claude, codex, codex_models, claude_models, ollama, named, anthropic, openai) = tokio::join!(
        cli::status(Cli::Claude),
        cli::status(Cli::Codex),
        cli::codex_models(),
        crate::discovery::claude(None),
        ollama_models(),
        futures::future::join_all(named.iter().map(crate::endpoints::backend)),
        api_backend("anthropic", "Anthropic API", None),
        api_backend("openai", "OpenAI API", None)
    );
    let mut out = vec![
        subscription(claude, claude_models.ok()),
        subscription(codex, codex_models),
    ];
    out.push(match ollama {
        Ok(models) => Backend {
            provider: "ollama".into(),
            label: "Ollama".into(),
            identity: None,
            kind: BackendKind::Local,
            ready: !models.is_empty(),
            detail: if models.is_empty() {
                "running, no models pulled".into()
            } else {
                "running".into()
            },
            fix: models
                .is_empty()
                .then(|| "ollama pull qwen3:4b-instruct".into()),
            models,
            model_options: vec![],
            catalog_state: Some(CatalogState::Current),
            step: None,
        },
        Err(detail) => Backend {
            provider: "ollama".into(),
            label: "Ollama".into(),
            identity: None,
            kind: BackendKind::Local,
            ready: false,
            detail,
            fix: Some("ollama serve".into()),
            models: vec![],
            model_options: vec![],
            catalog_state: Some(CatalogState::Unavailable),
            step: None,
        },
    });
    out.extend([anthropic, openai]);
    out.push(crate::opencode_go::backend().await);
    out.extend(named);
    match crate::accounts::list() {
        Ok(accounts) => {
            let extra = futures::stream::iter(accounts.into_iter().map(|account| {
                let base = out.iter().find(|b| b.provider == account.provider).cloned();
                async move {
                    let mut backend = base?;
                    if let Some(cli) = Cli::of(&account.provider) {
                        let status = cli::status_for(cli, Some(&account)).await;
                        let models = if status.signed_in {
                            match cli {
                                Cli::Codex => cli::codex_models_for(Some(&account)).await,
                                Cli::Claude => crate::discovery::claude(Some(&account)).await.ok(),
                            }
                        } else {
                            None
                        };
                        backend = subscription(status, models);
                    }
                    if Cli::of(&account.provider).is_none() {
                        backend.ready = account.state == crate::accounts::AccountState::Active;
                        backend.detail = "key saved".into();
                        backend.step = None;
                        backend.fix = None;
                    }
                    if matches!(account.provider.as_str(), "openai" | "anthropic") {
                        backend =
                            api_backend(&account.provider, &account.name, Some(&account)).await;
                    }
                    if account.provider == "opencode-go" {
                        backend = crate::opencode_go::backend_for(Some(&account)).await;
                    }
                    if let Some(mut endpoint) = crate::endpoints::endpoint(&account.provider) {
                        match account.key() {
                            Ok(key) => {
                                endpoint.key = Some(key);
                                endpoint.name = format!("{}:{}", account.id, account.revision);
                                backend = crate::endpoints::backend(&endpoint).await;
                            }
                            Err(error) => {
                                backend.ready = false;
                                backend.detail = error.to_string();
                            }
                        }
                    }
                    backend.provider = account.id.clone();
                    backend.label = account.name.clone();
                    if account.state != crate::accounts::AccountState::Active {
                        backend.ready = false;
                        backend.detail =
                            if account.state == crate::accounts::AccountState::Removing {
                                "Removing account"
                            } else {
                                "Reconnect required"
                            }
                            .into();
                    }
                    if crate::accounts::get(&account.id)
                        .ok()
                        .flatten()
                        .is_none_or(|current| {
                            current.revision != account.revision || current.state != account.state
                        })
                    {
                        return None;
                    }
                    Some(backend)
                }
            }));
            use futures::StreamExt;
            out.extend(
                extra
                    .buffer_unordered(4)
                    .filter_map(|b| async move { b })
                    .collect::<Vec<_>>()
                    .await,
            );
        }
        Err(error) => tracing::warn!(error = %error, "model accounts could not be listed"),
    }
    out
}

fn subscription(st: cli::CliStatus, advertised: Option<Vec<ModelOption>>) -> Backend {
    let cli = st.cli;
    let connected = st.installed && st.signed_in;
    let catalog_state = connected.then_some(if advertised.is_some() {
        CatalogState::Current
    } else {
        CatalogState::Unavailable
    });
    let model_options = advertised.unwrap_or_default();
    Backend {
        provider: cli.name().into(),
        label: cli.label().into(),
        identity: st.identity,
        kind: BackendKind::Subscription,
        ready: connected && !model_options.is_empty(),
        detail: if connected && catalog_state == Some(CatalogState::Unavailable) {
            "Could not load models. Try again.".into()
        } else if connected && model_options.is_empty() {
            "No models available".into()
        } else {
            st.detail
        },
        fix: if !st.installed {
            Some(cli.install_hint().into())
        } else if !st.signed_in {
            Some(cli.login_command().into())
        } else {
            None
        },
        models: model_options.iter().map(|m| m.id.clone()).collect(),
        model_options,
        catalog_state,
        step: if !st.installed {
            Some("install")
        } else if !st.signed_in {
            Some("sign_in")
        } else {
            None
        },
    }
}

async fn api_backend(
    provider: &str,
    label: &str,
    account: Option<&crate::accounts::ModelAccount>,
) -> Backend {
    let key_name = format!("{}_API_KEY", provider.to_uppercase());
    let present = account.map_or_else(
        || crate::keys::has_api_key(&key_name),
        |a| a.state == crate::accounts::AccountState::Active,
    );
    let listing = if present {
        let key = match account {
            Some(a) => a
                .key()
                .map_err(|_| "Could not read account key".to_string()),
            None => {
                crate::keys::api_key(&key_name).ok_or_else(|| "Could not read API key".to_string())
            }
        };
        match key {
            Ok(key) => {
                let base = if provider == "anthropic" {
                    std::env::var("ANTHROPIC_BASE_URL")
                        .unwrap_or_else(|_| "https://api.anthropic.com/v1".into())
                } else {
                    std::env::var("OPENAI_BASE_URL")
                        .unwrap_or_else(|_| "https://api.openai.com/v1".into())
                };
                crate::discovery::api(&base, &key, provider == "anthropic").await
            }
            Err(error) => Err(error),
        }
    } else {
        Ok(vec![])
    };
    let catalog_state = present.then_some(if listing.is_ok() {
        CatalogState::Current
    } else {
        CatalogState::Unavailable
    });
    let (detail, options) = match listing {
        Ok(models) if !present => ("No key".into(), models),
        Ok(models) if models.is_empty() => ("No models available".into(), models),
        Ok(models) => ("Key set".into(), models),
        Err(detail) => (detail, vec![]),
    };
    Backend {
        provider: provider.into(),
        label: label.into(),
        identity: None,
        kind: BackendKind::Api,
        ready: present && !options.is_empty(),
        detail,
        fix: None,
        models: options.iter().map(|m| m.id.clone()).collect(),
        model_options: options,
        catalog_state,
        step: None,
    }
}

async fn ollama_models() -> std::result::Result<Vec<String>, String> {
    let host = std::env::var("OLLAMA_HOST").unwrap_or_else(|_| "http://localhost:11434".into());
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{host}/api/tags"))
        .timeout(Duration::from_secs(2))
        .send()
        .await
        .map_err(|_| "not running".to_string())?;
    let v: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    let mut names: Vec<String> = v["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| m["name"].as_str().map(str::to_string))
        .collect();
    names.sort();
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status() -> cli::CliStatus {
        cli::CliStatus {
            cli: Cli::Codex,
            installed: true,
            signed_in: true,
            detail: "Connected".into(),
            identity: None,
        }
    }

    #[test]
    fn empty_and_unavailable_catalogs_never_reintroduce_suggested_models() {
        let empty = subscription(status(), Some(vec![]));
        assert_eq!(empty.catalog_state, Some(CatalogState::Current));
        assert_eq!(empty.detail, "No models available");
        assert!(!empty.ready);
        assert!(empty.models.is_empty());
        assert_eq!(empty.step, None);
        let unavailable = subscription(status(), None);
        assert_eq!(unavailable.catalog_state, Some(CatalogState::Unavailable));
        assert!(unavailable.models.is_empty());
        assert_eq!(unavailable.step, None);
        let mut absent = status();
        absent.installed = false;
        let absent = subscription(absent, None);
        assert_eq!(absent.catalog_state, None);
        assert_eq!(absent.step, Some("install"));
    }
}
