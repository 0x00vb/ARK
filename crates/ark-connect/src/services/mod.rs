//! The tools a connected capability gives, by the service name its catalog entry names. Each
//! service is an [`Adapter`] with neutral action names; it asks [`Connections`] for a token on
//! every call, so a refreshed or revoked sign-in applies at once.

use std::sync::Arc;

use ark_tools::{Adapter, AdapterError};

use crate::Connections;

pub mod google;

/// The adapter for `service`, acting as `account`. `None`: no such service.
pub fn adapter(service: &str, conns: Connections, account: String) -> Option<Arc<dyn Adapter>> {
    let ctx = Ctx {
        service: service.to_string(),
        conns,
        account,
    };
    match service {
        "google.mail" => Some(Arc::new(google::Mail(ctx))),
        "google.calendar" => Some(Arc::new(google::Calendar(ctx))),
        _ => None,
    }
}

/// The evidence card of a service's result (the runtime's distiller shape). `None` for an
/// action no service here offers.
pub fn distill(action: &str, out: &serde_json::Value) -> Option<(String, Vec<String>)> {
    google::distill(action, out)
}

/// What a service needs to call its API as an account.
pub struct Ctx {
    service: String,
    conns: Connections,
    account: String,
}

impl Ctx {
    /// The API's base URL: `default`, or the one the connections were given for this service.
    pub fn base<'a>(&'a self, default: &'a str) -> &'a str {
        self.conns.base(&self.service).unwrap_or(default)
    }

    /// Send a request with the account's token. A refused token is dropped and the request
    /// sent once more with a new one.
    pub async fn send(
        &self,
        build: impl Fn(&reqwest::Client) -> reqwest::RequestBuilder,
    ) -> Result<serde_json::Value, AdapterError> {
        for attempt in 0..2 {
            let token = self
                .conns
                .access_token(&self.account)
                .await
                .map_err(|e| AdapterError::Tool(e.to_string()))?;
            let resp = build(self.conns.http())
                .bearer_auth(token)
                .send()
                .await
                .map_err(|e| AdapterError::Tool(e.to_string()))?;
            let status = resp.status();
            if status.as_u16() == 401 && attempt == 0 {
                self.conns.expire(&self.account);
                continue;
            }
            let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
            if !status.is_success() {
                let why = body["error"]["message"]
                    .as_str()
                    .unwrap_or("no reason given")
                    .to_string();
                return Err(AdapterError::Tool(format!("{status}: {why}")));
            }
            return Ok(body);
        }
        Err(AdapterError::Tool(
            "the account's sign-in was refused; connect it again".into(),
        ))
    }
}
