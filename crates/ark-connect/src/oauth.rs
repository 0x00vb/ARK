//! Signing in to a provider: OAuth 2.0 authorization code with PKCE and a loopback redirect
//! (RFC 8252, RFC 7636). ARK listens on `127.0.0.1` on a free port, the person signs in in
//! their own browser, the provider sends the browser back to that port with a code, and ARK
//! trades the code for tokens. No password passes through ARK.

use std::time::{Duration, SystemTime};

use base64::Engine as _;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::catalog::Provider;
use crate::ConnectError;

/// What a provider gave back.
#[derive(Clone, Debug, Default)]
pub struct Tokens {
    pub access: String,
    pub refresh: Option<String>,
    pub expires_at: Option<SystemTime>,
    /// The scopes granted, when the provider says.
    pub scopes: Vec<String>,
    pub id_token: Option<String>,
}

impl Tokens {
    /// Still good for at least a minute.
    pub fn fresh(&self) -> bool {
        self.expires_at
            .is_none_or(|t| t > SystemTime::now() + Duration::from_secs(60))
    }
}

/// A sign-in in progress: open [`url`](SignIn::url) in the person's browser, then
/// [`finish`](SignIn::finish).
pub struct SignIn {
    url: String,
    listener: TcpListener,
    state: String,
    verifier: String,
    redirect: String,
    provider: Provider,
    client_id: String,
    landing: Option<String>,
}

/// Start a sign-in for `scopes` (the provider's identity scopes are added).
pub async fn begin(provider: &Provider, scopes: &[String]) -> Result<SignIn, ConnectError> {
    let client_id = provider.oauth.client_id().ok_or_else(|| {
        ConnectError::NotSetUp(format!(
            "this build of ARK has no sign-in for {}",
            provider.label
        ))
    })?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let redirect = format!("http://127.0.0.1:{}/", listener.local_addr()?.port());
    let state = token();
    let verifier = format!("{}{}", token(), token());
    let challenge = b64(&Sha256::digest(verifier.as_bytes()));
    let mut all: Vec<String> = provider.oauth.identity_scopes.clone();
    for s in scopes {
        if !all.contains(s) {
            all.push(s.clone());
        }
    }
    let mut url = reqwest::Url::parse(&provider.oauth.auth_url)
        .map_err(|e| ConnectError::Provider(format!("auth_url: {e}")))?;
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("response_type", "code")
            .append_pair("client_id", &client_id)
            .append_pair("redirect_uri", &redirect)
            .append_pair("scope", &all.join(" "))
            .append_pair("state", &state)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256");
        for (k, v) in &provider.oauth.params {
            q.append_pair(k, v);
        }
    }
    Ok(SignIn {
        url: url.to_string(),
        listener,
        state,
        verifier,
        redirect,
        provider: provider.clone(),
        client_id,
        landing: None,
    })
}

impl SignIn {
    /// The page the person signs in on.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Where the browser goes once it is back with a code: a page of the provider's own
    /// ("You're signed in") instead of ARK's plain one. Refusals still show ARK's page.
    pub fn landing(mut self, url: impl Into<String>) -> Self {
        self.landing = Some(url.into());
        self
    }

    /// Wait for the browser to come back (at most `within`), then trade the code for tokens.
    pub async fn finish(
        self,
        http: &reqwest::Client,
        within: Duration,
    ) -> Result<Tokens, ConnectError> {
        let code = tokio::time::timeout(within, self.wait_for_code())
            .await
            .map_err(|_| {
                ConnectError::Cancelled("the sign-in was not finished in time".into())
            })??;
        let mut form = vec![
            ("grant_type", "authorization_code".to_string()),
            ("code", code),
            ("redirect_uri", self.redirect.clone()),
            ("client_id", self.client_id.clone()),
            ("code_verifier", self.verifier.clone()),
        ];
        if let Some(secret) = self.provider.oauth.client_secret() {
            form.push(("client_secret", secret));
        }
        token_request(http, &self.provider, &form).await
    }

    async fn wait_for_code(&self) -> Result<String, ConnectError> {
        loop {
            let (mut sock, _) = self.listener.accept().await?;
            let mut buf = vec![0u8; 8192];
            let n = sock.read(&mut buf).await.unwrap_or(0);
            let head = String::from_utf8_lossy(&buf[..n]);
            let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
            let url = reqwest::Url::parse(&format!("http://127.0.0.1{path}"))
                .map_err(|e| ConnectError::Provider(e.to_string()))?;
            let get = |k: &str| {
                url.query_pairs()
                    .find(|(n, _)| n == k)
                    .map(|(_, v)| v.to_string())
            };
            let (code, error, state) = (get("code"), get("error"), get("state"));
            if code.is_none() && error.is_none() {
                // A favicon or another request that is not the redirect.
                reply(&mut sock, 404, "").await;
                continue;
            }
            if state.as_deref() != Some(self.state.as_str()) {
                reply(&mut sock, 400, &page("This sign-in link is not the one ARK started. Go back to ARK and try again.")).await;
                return Err(ConnectError::Provider(
                    "the sign-in came back with another state".into(),
                ));
            }
            if let Some(e) = error {
                reply(
                    &mut sock,
                    200,
                    &page("Not connected. You can close this tab and go back to ARK."),
                )
                .await;
                return Err(ConnectError::Cancelled(format!(
                    "the sign-in was not allowed ({e})"
                )));
            }
            match &self.landing {
                Some(to) => redirect(&mut sock, to).await,
                None => {
                    reply(
                        &mut sock,
                        200,
                        &page("Connected. You can close this tab and go back to ARK."),
                    )
                    .await
                }
            }
            return Ok(code.unwrap_or_default());
        }
    }
}

/// New tokens from a refresh token. The provider may or may not send a new refresh token.
pub async fn refresh(
    http: &reqwest::Client,
    provider: &Provider,
    refresh_token: &str,
) -> Result<Tokens, ConnectError> {
    let client_id = provider
        .oauth
        .client_id()
        .ok_or_else(|| ConnectError::NotSetUp(format!("no client for {}", provider.label)))?;
    let mut form = vec![
        ("grant_type", "refresh_token".to_string()),
        ("refresh_token", refresh_token.to_string()),
        ("client_id", client_id),
    ];
    if let Some(secret) = provider.oauth.client_secret() {
        form.push(("client_secret", secret));
    }
    token_request(http, provider, &form).await
}

/// Ask the provider to forget a token (best effort: a token it no longer knows is fine).
pub async fn revoke(
    http: &reqwest::Client,
    provider: &Provider,
    token: &str,
) -> Result<(), ConnectError> {
    let Some(url) = &provider.oauth.revoke_url else {
        return Ok(());
    };
    // A public client names itself (RFC 7009 §2.1); a provider that does not need it ignores it.
    let mut form = vec![("token", token.to_string())];
    if let Some(id) = provider.oauth.client_id() {
        form.push(("client_id", id));
    }
    let resp = http.post(url).form(&form).send().await?;
    if resp.status().is_success() || resp.status().as_u16() == 400 {
        Ok(())
    } else {
        Err(ConnectError::Provider(format!(
            "{} answered {} to the revoke",
            provider.label,
            resp.status()
        )))
    }
}

/// A claim of an ID token's payload (`email`). The token came straight from the provider over
/// TLS, so its signature is not checked here.
pub fn claim(id_token: &str, name: &str) -> Option<String> {
    let payload = id_token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    let v: Value = serde_json::from_slice(&bytes).ok()?;
    v.get(name)?.as_str().map(str::to_string)
}

async fn token_request(
    http: &reqwest::Client,
    provider: &Provider,
    form: &[(&str, String)],
) -> Result<Tokens, ConnectError> {
    let resp = http
        .post(&provider.oauth.token_url)
        .form(form)
        .send()
        .await?;
    let status = resp.status();
    let v: Value = resp.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        let why = v["error_description"]
            .as_str()
            .or(v["error"].as_str())
            .unwrap_or("no reason given");
        let text = format!("{} refused the sign-in ({status}): {why}", provider.label);
        // A 4xx is the provider's answer (an expired or revoked grant): asking again will not
        // help. Anything else may pass.
        return Err(if status.is_client_error() {
            ConnectError::Refused(text)
        } else {
            ConnectError::Provider(text)
        });
    }
    let access = v["access_token"]
        .as_str()
        .ok_or_else(|| ConnectError::Provider("no access token in the reply".into()))?;
    Ok(Tokens {
        access: access.to_string(),
        refresh: v["refresh_token"].as_str().map(str::to_string),
        expires_at: v["expires_in"]
            .as_u64()
            .map(|s| SystemTime::now() + Duration::from_secs(s)),
        scopes: v["scope"]
            .as_str()
            .map(|s| s.split_whitespace().map(str::to_string).collect())
            .unwrap_or_default(),
        id_token: v["id_token"].as_str().map(str::to_string),
    })
}

fn token() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn page(text: &str) -> String {
    format!(
        "<!doctype html><meta charset=utf-8><title>ARK</title><body style=\"font:16px system-ui;margin:4em auto;max-width:32em\"><p>{text}</p></body>"
    )
}

async fn redirect(sock: &mut tokio::net::TcpStream, to: &str) {
    let text = format!(
        "HTTP/1.1 303 See Other\r\nlocation: {to}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
    );
    let _ = sock.write_all(text.as_bytes()).await;
}

async fn reply(sock: &mut tokio::net::TcpStream, status: u16, body: &str) {
    let text = format!(
        "HTTP/1.1 {status} OK\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = sock.write_all(text.as_bytes()).await;
}
