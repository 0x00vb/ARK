//! What ARK knows how to connect, as data: providers (where an account lives and how to sign
//! in) and capabilities (what a person sees, the tools they give, what they need). The built-in
//! catalog is `catalog.json`; an app can bring its own.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The built-in catalog.
pub const BUILTIN: &str = include_str!("catalog.json");

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Catalog {
    #[serde(default)]
    pub providers: Vec<Provider>,
    #[serde(default)]
    pub capabilities: Vec<Capability>,
}

/// A capability as a person sees it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Capability {
    /// Stable id (`mail`).
    pub id: String,
    /// What the person reads (`Mail`).
    pub label: String,
    /// One line on what it lets ARK do.
    pub says: String,
    /// The action namespaces it gives (`mail` gives `mail.send`).
    pub tools: Vec<String>,
    /// The provider whose account it needs. `None`: it needs no account (Web, Files).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Which built-in service implements its tools on that provider (`google.mail`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
    /// The scopes it needs on the account.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
}

/// Where accounts live and how ARK signs in to one.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Provider {
    pub id: String,
    pub label: String,
    pub oauth: OAuth,
}

/// OAuth 2.0 authorization code with PKCE and a loopback redirect (RFC 8252), the way a
/// desktop app signs in.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OAuth {
    pub auth_url: String,
    pub token_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoke_url: Option<String>,
    /// ARK's client id for this provider. Empty: not registered yet; `client_id_env` may set it.
    #[serde(default)]
    pub client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id_env: Option<String>,
    /// A desktop client's secret is not secret (RFC 8252 §8.5); some providers still want it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret_env: Option<String>,
    /// Scopes asked with every sign-in, to learn which account it is (`openid`, `email`).
    #[serde(default)]
    pub identity_scopes: Vec<String>,
    /// Extra query parameters of the authorization request.
    #[serde(default)]
    pub params: BTreeMap<String, String>,
}

impl OAuth {
    /// The client id to use: the environment's when set, else the catalog's. `None`: this build
    /// has no client for the provider.
    pub fn client_id(&self) -> Option<String> {
        self.client_id_env
            .as_deref()
            .and_then(|k| std::env::var(k).ok())
            .filter(|v| !v.trim().is_empty())
            .or_else(|| Some(self.client_id.clone()).filter(|v| !v.trim().is_empty()))
    }

    pub fn client_secret(&self) -> Option<String> {
        self.client_secret_env
            .as_deref()
            .and_then(|k| std::env::var(k).ok())
            .filter(|v| !v.trim().is_empty())
            .or_else(|| self.client_secret.clone())
    }
}

impl Catalog {
    pub fn builtin() -> Catalog {
        serde_json::from_str(BUILTIN).expect("the built-in catalog is valid")
    }

    pub fn capability(&self, id: &str) -> Option<&Capability> {
        let id = id.trim().to_ascii_lowercase();
        self.capabilities
            .iter()
            .find(|c| c.id == id || c.label.to_ascii_lowercase() == id)
    }

    pub fn provider(&self, id: &str) -> Option<&Provider> {
        self.providers.iter().find(|p| p.id == id)
    }

    /// The capability whose tools include `action` (`mail.send` → Mail).
    pub fn capability_of(&self, action: &str) -> Option<&Capability> {
        let ns = action.split('.').next()?;
        self.capabilities
            .iter()
            .find(|c| c.tools.iter().any(|t| t == ns))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_builtin_catalog_reads_and_finds_by_id_label_and_action() {
        let c = Catalog::builtin();
        assert_eq!(c.capability("Mail").unwrap().id, "mail");
        assert_eq!(
            c.capability_of("calendar.create").unwrap().label,
            "Calendar"
        );
        let mail = c.capability("mail").unwrap();
        let google = c.provider(mail.provider.as_deref().unwrap()).unwrap();
        assert!(google.oauth.identity_scopes.contains(&"email".to_string()));
        assert!(c.capability("web").unwrap().provider.is_none());
    }
}
