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
    /// A subscription's program not ready: the step it needs, `install` or `sign_in`, which a
    /// window can run on the computer it lists (ADR 0052).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<&'static str>,
}

fn bare(model: &str) -> &str {
    model.strip_suffix(":latest").unwrap_or(model)
}

static RETIRED: std::sync::Mutex<Vec<(String, String)>> = std::sync::Mutex::new(Vec::new());

/// Remember, for the rest of the process, that a provider answered 410 for this model.
pub(crate) fn retire(provider: &str, model: &str) {
    let mut set = RETIRED.lock().unwrap_or_else(|e| e.into_inner());
    if !set
        .iter()
        .any(|(p, m)| p == provider && bare(m) == bare(model))
    {
        set.push((provider.to_string(), model.to_string()));
    }
}

fn without_retired(backends: &mut [Backend]) {
    let set = RETIRED.lock().unwrap_or_else(|e| e.into_inner());
    if set.is_empty() {
        return;
    }
    for b in backends {
        let gone = |m: &str| {
            set.iter()
                .any(|(p, r)| *p == b.provider && bare(r) == bare(m))
        };
        let kept: Vec<String> = b.models.iter().filter(|m| !gone(m)).cloned().collect();
        b.models = kept;
        let kept: Vec<_> = b
            .model_options
            .iter()
            .filter(|o| !gone(&o.id))
            .cloned()
            .collect();
        b.model_options = kept;
    }
}

/// Check every backend. Runs the checks at once; a slow or missing one costs at most a few
/// seconds and never fails the listing.
pub async fn catalog() -> Vec<Backend> {
    let named = crate::endpoints::endpoints();
    let (claude, codex, codex_models, ollama, named) = tokio::join!(
        cli::status(Cli::Claude),
        cli::status(Cli::Codex),
        cli::codex_models(),
        ollama_models(),
        futures::future::join_all(named.iter().map(crate::endpoints::backend))
    );
    let mut out = vec![
        subscription(claude, None),
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
            step: None,
        },
    });
    for (provider, label, key, model) in [
        (
            "anthropic",
            "Anthropic API",
            "ANTHROPIC_API_KEY",
            "claude-sonnet-5-5",
        ),
        ("openai", "OpenAI API", "OPENAI_API_KEY", "gpt-5"),
    ] {
        let ready = crate::keys::has_api_key(key);
        out.push(Backend {
            provider: provider.into(),
            label: label.into(),
            identity: None,
            kind: BackendKind::Api,
            ready,
            detail: if ready {
                "key set".into()
            } else {
                "no key".into()
            },
            fix: None,
            models: vec![model.into()],
            model_options: vec![],
            step: None,
        });
    }
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
                        let models = if cli == Cli::Codex && status.signed_in {
                            cli::codex_models_for(Some(&account)).await
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
    without_retired(&mut out);
    out
}

fn subscription(st: cli::CliStatus, advertised: Option<Vec<ModelOption>>) -> Backend {
    let cli = st.cli;
    let model_options = advertised.unwrap_or_else(|| {
        cli.suggested_models()
            .iter()
            .map(|model| ModelOption {
                id: (*model).into(),
                label: cli.model_label(model),
                // Codex efforts are model-specific. If live discovery failed, Default still
                // works but the picker must not advertise unverified effort choices.
                efforts: if cli == Cli::Codex {
                    vec![]
                } else {
                    cli.efforts().iter().map(|e| (*e).into()).collect()
                },
                default_effort: None,
            })
            .collect()
    });
    Backend {
        provider: cli.name().into(),
        label: cli.label().into(),
        identity: st.identity,
        kind: BackendKind::Subscription,
        ready: st.installed && st.signed_in,
        detail: st.detail,
        fix: if !st.installed {
            Some(cli.install_hint().into())
        } else if !st.signed_in {
            Some(cli.login_command().into())
        } else {
            None
        },
        models: model_options.iter().map(|m| m.id.clone()).collect(),
        model_options,
        step: if !st.installed {
            Some("install")
        } else if !st.signed_in {
            Some("sign_in")
        } else {
            None
        },
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
