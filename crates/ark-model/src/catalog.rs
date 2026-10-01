//! What models this machine can use right now: signed-in subscriptions, local Ollama models,
//! and APIs with a key in the environment. Front-ends build their model picker from it.

use crate::cli::{self, Cli};
use serde::Serialize;
use std::time::Duration;

/// Provider names a model spec may start with (`claude:sonnet`, `ollama qwen3:4b`).
pub const PROVIDERS: &[&str] = &["ollama", "openai", "anthropic", "claude", "codex", "mock"];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BackendKind {
    Subscription,
    Local,
    Api,
}

/// One way to reach models.
#[derive(Clone, Debug, Serialize)]
pub struct Backend {
    /// The provider name in a model spec.
    pub provider: String,
    pub label: String,
    pub kind: BackendKind,
    /// Usable now.
    pub ready: bool,
    /// How it is connected, or why it is not.
    pub detail: String,
    /// A command that makes it ready (sign in, install), when it is not.
    pub fix: Option<String>,
    /// Models to offer. Others can still be typed as a spec.
    pub models: Vec<String>,
}

/// Check every backend. Runs the checks at once; a slow or missing one costs at most a few
/// seconds and never fails the listing.
pub async fn catalog() -> Vec<Backend> {
    let (claude, codex, ollama) = tokio::join!(
        cli::status(Cli::Claude),
        cli::status(Cli::Codex),
        ollama_models()
    );
    let mut out: Vec<Backend> = [claude, codex].into_iter().map(subscription).collect();
    out.push(match ollama {
        Ok(models) => Backend {
            provider: "ollama".into(),
            label: "Ollama".into(),
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
        },
        Err(detail) => Backend {
            provider: "ollama".into(),
            label: "Ollama".into(),
            kind: BackendKind::Local,
            ready: false,
            detail,
            fix: Some("ollama serve".into()),
            models: vec![],
        },
    });
    for (provider, label, key, model) in [
        (
            "anthropic",
            "Anthropic API",
            "ANTHROPIC_API_KEY",
            "claude-sonnet-5-5",
        ),
        ("openai", "OpenAI API", "OPENAI_API_KEY", "gpt-4o-mini"),
    ] {
        let ready = std::env::var(key).is_ok_and(|k| !k.trim().is_empty());
        out.push(Backend {
            provider: provider.into(),
            label: label.into(),
            kind: BackendKind::Api,
            ready,
            detail: if ready {
                format!("{key} set")
            } else {
                format!("{key} not set")
            },
            fix: None,
            models: vec![model.into()],
        });
    }
    out
}

fn subscription(st: cli::CliStatus) -> Backend {
    let cli = st.cli;
    Backend {
        provider: cli.name().into(),
        label: cli.label().into(),
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
        models: cli
            .suggested_models()
            .iter()
            .map(|m| m.to_string())
            .collect(),
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
