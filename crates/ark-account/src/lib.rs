//! # ark-account
//!
//! An ARK Account on this computer. Optional: ARK runs agents locally with no account, and
//! nothing in the runtime asks for one. An account is who ARK Cloud serves (later: cloud
//! agents, plans, usage), so the Cloud features of an app ask this crate one question:
//! [`Account::can`] (is someone signed in, and may they use this?).
//!
//! Signing in is OAuth 2.1 with PKCE and a loopback redirect (RFC 8252), the same sign-in
//! `ark-connect` uses for Google: the person signs in on the account service's page in their
//! own browser, and the browser comes back to `127.0.0.1` with a code. The refresh token lives
//! in the OS keychain ([`ark_connect::Secrets`]); access tokens live in memory and last minutes.
//! Every computer is its own sign-in: signing out here revokes this computer's token only.
//!
//! The service is any OpenID Connect provider that serves ARK's account API (`/v1/me`):
//! [`Service::ark`] is ARK's own (`account.aiark.dev`); `ARK_ACCOUNT_URL` points an app at
//! another one (a local or self-hosted service). Nothing here names an auth library.
//!
//! ```no_run
//! # async fn demo() -> Result<(), ark_account::AccountError> {
//! use std::{sync::Arc, time::Duration};
//! use ark_account::{Access, Account, Service};
//!
//! let account = Account::new(Service::ark("ark-desktop"), Arc::new(ark_connect::Keychain));
//! if !account.status().signed_in() {
//!     let signing_in = account.sign_in().await?;
//!     // the app opens signing_in.url() in the browser
//!     let profile = signing_in.finish(Duration::from_secs(600)).await?;
//!     println!("signed in as {}", profile.email);
//! }
//! assert_eq!(account.can("cloud.agents"), Access::NotIncluded); // no plans yet
//! # Ok(()) }
//! ```

use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use ark_connect::catalog::{OAuth, Provider};
use ark_connect::oauth;
use ark_connect::{ConnectError, Secrets};
use serde::{Deserialize, Serialize};

/// ARK's account service, the OAuth issuer.
pub const ARK_ISSUER: &str = "https://account.aiark.dev";
/// The ARK Cloud API: the audience of access tokens.
pub const ARK_API: &str = "https://api.aiark.dev";

/// Scopes an app asks for: who the person is, and `offline_access` to stay signed in.
const SCOPES: [&str; 4] = ["openid", "profile", "email", "offline_access"];

/// Where the account lives and which app is signing in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Service {
    /// The issuer: the service's origin, which serves `/.well-known/openid-configuration`
    /// and `/v1/me`.
    pub issuer: String,
    /// What access tokens are for (the RFC 8707 `resource`).
    pub audience: String,
    /// The app's client id at the service (`ark-desktop`). A public client: no secret.
    pub client_id: String,
}

impl Service {
    /// ARK's own service, for the app `client_id`. `ARK_ACCOUNT_URL` and `ARK_API_AUDIENCE`
    /// point it elsewhere (a local or self-hosted service).
    pub fn ark(client_id: &str) -> Self {
        let env = |k: &str, default: &str| {
            std::env::var(k)
                .ok()
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| default.to_string())
        };
        Self::new(
            env("ARK_ACCOUNT_URL", ARK_ISSUER),
            env("ARK_API_AUDIENCE", ARK_API),
            client_id,
        )
    }

    pub fn new(issuer: impl Into<String>, audience: impl Into<String>, client_id: &str) -> Self {
        Self {
            issuer: issuer.into().trim_end_matches('/').to_string(),
            audience: audience.into().trim_end_matches('/').to_string(),
            client_id: client_id.to_string(),
        }
    }

    /// The keychain entry for this service's sign-in.
    fn key(&self) -> String {
        format!("account:{}", self.issuer)
    }
}

/// The signed-in account, as the service describes it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    /// Stable id. The account's identity everywhere; the email can change.
    pub id: String,
    pub email: String,
    pub name: String,
    #[serde(default)]
    pub image: Option<String>,
    /// What this account may use in ARK Cloud (`cloud.agents`). The service decides.
    #[serde(default)]
    pub entitlements: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Status {
    SignedOut,
    SignedIn { profile: Profile },
}

impl Status {
    pub fn signed_in(&self) -> bool {
        matches!(self, Status::SignedIn { .. })
    }

    pub fn profile(&self) -> Option<&Profile> {
        match self {
            Status::SignedIn { profile } => Some(profile),
            Status::SignedOut => None,
        }
    }
}

/// The answer to "may this account use this?".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    Allowed,
    /// Nobody is signed in: offer Sign in.
    SignIn,
    /// Signed in, but the account does not include it.
    NotIncluded,
}

#[derive(Debug, thiserror::Error)]
pub enum AccountError {
    #[error("Not signed in.")]
    NotSignedIn,
    #[error("ARK's account service could not be reached. Check the connection and try again.")]
    Unreachable,
    #[error("The sign-in was not finished.")]
    Cancelled,
    #[error("{0}")]
    Service(String),
    #[error("The system keychain refused: {0}")]
    Secrets(String),
}

impl From<ConnectError> for AccountError {
    fn from(e: ConnectError) -> Self {
        match e {
            ConnectError::Cancelled(_) => AccountError::Cancelled,
            ConnectError::Secrets(s) => AccountError::Secrets(s),
            ConnectError::Http(_) | ConnectError::Io(_) => AccountError::Unreachable,
            ConnectError::Refused(_) => AccountError::NotSignedIn,
            other => AccountError::Service(other.to_string()),
        }
    }
}

impl From<reqwest::Error> for AccountError {
    fn from(_: reqwest::Error) -> Self {
        AccountError::Unreachable
    }
}

/// What the keychain keeps: the refresh token, and the profile so the app knows who is signed
/// in before the network answers. No `Debug`: it holds a token.
#[derive(Serialize, Deserialize)]
struct Saved {
    refresh_token: String,
    profile: Profile,
}

/// The account on this computer. Cheap to clone; clones share state.
#[derive(Clone)]
pub struct Account(Arc<Shared>);

type Listener = Box<dyn Fn(&Status) + Send + Sync>;

struct Shared {
    service: Service,
    secrets: Arc<dyn Secrets>,
    http: reqwest::Client,
    state: Mutex<State>,
    /// One refresh at a time: refresh tokens rotate, so two at once would spend one twice.
    refreshing: tokio::sync::Mutex<()>,
    on_change: Mutex<Vec<Listener>>,
}

#[derive(Default)]
struct State {
    profile: Option<Profile>,
    access: Option<(String, SystemTime)>,
    provider: Option<Provider>,
}

impl Account {
    /// The account as the keychain left it. No network: [`refresh`](Account::refresh) checks
    /// it with the service.
    pub fn new(service: Service, secrets: Arc<dyn Secrets>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .unwrap_or_default();
        let account = Account(Arc::new(Shared {
            service,
            secrets,
            http,
            state: Mutex::new(State::default()),
            refreshing: tokio::sync::Mutex::new(()),
            on_change: Mutex::new(Vec::new()),
        }));
        match account.saved() {
            Ok(Some(saved)) => account.0.state.lock().unwrap().profile = Some(saved.profile),
            Ok(None) => {}
            Err(e) => tracing::warn!("account: reading the keychain: {e}"),
        }
        account
    }

    pub fn service(&self) -> &Service {
        &self.0.service
    }

    /// Who is signed in, as last known. Never waits for the network.
    pub fn status(&self) -> Status {
        match &self.0.state.lock().unwrap().profile {
            Some(p) => Status::SignedIn { profile: p.clone() },
            None => Status::SignedOut,
        }
    }

    /// May the signed-in account use `entitlement` (`cloud.agents`)? The one check Cloud
    /// features make; local features make none.
    pub fn can(&self, entitlement: &str) -> Access {
        match &self.0.state.lock().unwrap().profile {
            None => Access::SignIn,
            Some(p) if p.entitlements.iter().any(|e| e == entitlement) => Access::Allowed,
            Some(_) => Access::NotIncluded,
        }
    }

    /// Call `f` whenever the status changes (signed in, out, or the profile changed).
    pub fn on_change(&self, f: impl Fn(&Status) + Send + Sync + 'static) {
        self.0.on_change.lock().unwrap().push(Box::new(f));
    }

    /// Start signing in: open [`SigningIn::url`] in the person's browser, then
    /// [`finish`](SigningIn::finish).
    pub async fn sign_in(&self) -> Result<SigningIn, AccountError> {
        let provider = self.provider().await?;
        let sign_in = oauth::begin(&provider, &[])
            .await?
            .landing(format!("{}/signed-in", self.0.service.issuer));
        Ok(SigningIn {
            account: self.clone(),
            sign_in,
        })
    }

    /// Check the sign-in with the service and bring the profile (and its entitlements) up to
    /// date. Run it when the app starts. A token the service refused signs out; an
    /// unreachable service keeps the last known status.
    pub async fn refresh(&self) -> Result<Status, AccountError> {
        if self.saved()?.is_none() {
            return Ok(Status::SignedOut);
        }
        let access = self.access_token().await?;
        let profile = match self.fetch_profile(&access).await {
            // A token the service just issued, refused: the account is gone.
            Err(AccountError::NotSignedIn) => {
                self.forget()?;
                return Err(AccountError::NotSignedIn);
            }
            other => other?,
        };
        self.update_profile(profile)?;
        Ok(self.status())
    }

    /// An access token for ARK Cloud's API, refreshed when it is about to expire. Never log
    /// it. [`AccountError::NotSignedIn`] when there is no sign-in or the service ended it.
    pub async fn access_token(&self) -> Result<String, AccountError> {
        if let Some(t) = self.fresh_access() {
            return Ok(t);
        }
        let _one = self.0.refreshing.lock().await;
        if let Some(t) = self.fresh_access() {
            return Ok(t);
        }
        let saved = self.saved()?.ok_or(AccountError::NotSignedIn)?;
        let provider = self.provider().await?;
        let tokens = match oauth::refresh(&self.0.http, &provider, &saved.refresh_token).await {
            Ok(t) => t,
            Err(ConnectError::Refused(why)) => {
                // Revoked, expired, or signed out elsewhere: this computer is signed out.
                tracing::info!("account: the service ended this sign-in ({why})");
                self.forget()?;
                return Err(AccountError::NotSignedIn);
            }
            Err(e) => return Err(e.into()),
        };
        // The refresh token rotates: keep the new one before anything else can fail.
        if let Some(next) = &tokens.refresh {
            self.save(&Saved {
                refresh_token: next.clone(),
                profile: saved.profile,
            })?;
        }
        let expires = tokens
            .expires_at
            .unwrap_or_else(|| SystemTime::now() + Duration::from_secs(300));
        self.0.state.lock().unwrap().access = Some((tokens.access.clone(), expires));
        Ok(tokens.access)
    }

    /// Sign this computer out: forget the tokens here, then ask the service to revoke them.
    /// Signed out locally even when the service cannot be reached.
    pub async fn sign_out(&self) -> Result<(), AccountError> {
        let saved = self.saved()?;
        self.forget()?;
        if let Some(saved) = saved {
            self.revoke(&saved.refresh_token).await;
        }
        Ok(())
    }

    async fn revoke(&self, refresh_token: &str) {
        let done = match self.provider().await {
            Ok(p) => oauth::revoke(&self.0.http, &p, refresh_token)
                .await
                .map_err(AccountError::from),
            Err(e) => Err(e),
        };
        if let Err(e) = done {
            // The token stays valid at the service until it expires (90 days) or is revoked
            // from another device. Nothing on this computer can use it any more.
            tracing::warn!("account: revoking at the service: {e}");
        }
    }

    fn fresh_access(&self) -> Option<String> {
        let st = self.0.state.lock().unwrap();
        st.access
            .as_ref()
            .filter(|(_, exp)| *exp > SystemTime::now() + Duration::from_secs(60))
            .map(|(t, _)| t.clone())
    }

    async fn fetch_profile(&self, access: &str) -> Result<Profile, AccountError> {
        let resp = self
            .0
            .http
            .get(format!("{}/v1/me", self.0.service.issuer))
            .bearer_auth(access)
            .send()
            .await?;
        match resp.status().as_u16() {
            200 => resp
                .json()
                .await
                .map_err(|e| AccountError::Service(format!("the account service answered: {e}"))),
            401 => {
                self.0.state.lock().unwrap().access = None;
                Err(AccountError::NotSignedIn)
            }
            s => Err(AccountError::Service(format!(
                "the account service answered {s}"
            ))),
        }
    }

    /// The service's endpoints, from its discovery document (fetched once).
    async fn provider(&self) -> Result<Provider, AccountError> {
        if let Some(p) = &self.0.state.lock().unwrap().provider {
            return Ok(p.clone());
        }
        let s = &self.0.service;
        let url = format!("{}/.well-known/openid-configuration", s.issuer);
        let resp = self.0.http.get(&url).send().await?;
        if !resp.status().is_success() {
            return Err(AccountError::Service(format!(
                "the account service answered {} for its configuration",
                resp.status()
            )));
        }
        let d: serde_json::Value = resp.json().await?;
        // A document for another issuer is a misconfiguration or a mix-up: never sign in there.
        if d["issuer"].as_str().map(|i| i.trim_end_matches('/')) != Some(s.issuer.as_str()) {
            return Err(AccountError::Service(format!("{url} names another issuer")));
        }
        let field = |k: &str| {
            d[k].as_str()
                .map(str::to_string)
                .ok_or_else(|| AccountError::Service(format!("the account service has no {k}")))
        };
        let provider = Provider {
            id: "ark-account".into(),
            label: "ARK".into(),
            oauth: OAuth {
                auth_url: field("authorization_endpoint")?,
                token_url: field("token_endpoint")?,
                revoke_url: d["revocation_endpoint"].as_str().map(str::to_string),
                client_id: s.client_id.clone(),
                client_id_env: None,
                client_secret: None,
                client_secret_env: None,
                identity_scopes: SCOPES.iter().map(|s| s.to_string()).collect(),
                params: [
                    ("resource".to_string(), s.audience.clone()),
                    // Always ask in the browser, so a second person or account can sign in on
                    // a browser that is already signed in.
                    ("prompt".to_string(), "login".to_string()),
                ]
                .into(),
            },
        };
        self.0.state.lock().unwrap().provider = Some(provider.clone());
        Ok(provider)
    }

    fn saved(&self) -> Result<Option<Saved>, AccountError> {
        let raw = self.0.secrets.get(&self.0.service.key())?;
        Ok(raw.and_then(|r| serde_json::from_str(&r).ok()))
    }

    fn save(&self, saved: &Saved) -> Result<(), AccountError> {
        let raw = serde_json::to_string(saved).expect("serializes");
        self.0.secrets.set(&self.0.service.key(), &raw)?;
        Ok(())
    }

    fn update_profile(&self, profile: Profile) -> Result<(), AccountError> {
        let Some(saved) = self.saved()? else {
            return Err(AccountError::NotSignedIn);
        };
        let changed = self.0.state.lock().unwrap().profile.as_ref() != Some(&profile);
        self.save(&Saved {
            refresh_token: saved.refresh_token,
            profile: profile.clone(),
        })?;
        self.0.state.lock().unwrap().profile = Some(profile);
        if changed {
            self.changed();
        }
        Ok(())
    }

    fn forget(&self) -> Result<(), AccountError> {
        self.0.secrets.delete(&self.0.service.key())?;
        let was = {
            let mut st = self.0.state.lock().unwrap();
            st.access = None;
            st.profile.take().is_some()
        };
        if was {
            self.changed();
        }
        Ok(())
    }

    fn changed(&self) {
        let status = self.status();
        for f in self.0.on_change.lock().unwrap().iter() {
            f(&status);
        }
    }
}

/// A sign-in in progress.
pub struct SigningIn {
    account: Account,
    sign_in: oauth::SignIn,
}

impl SigningIn {
    /// The page to open in the person's browser.
    pub fn url(&self) -> &str {
        self.sign_in.url()
    }

    /// Wait for the browser to come back (at most `within`), then keep the sign-in. A sign-in
    /// that replaces another on this computer revokes the old one.
    pub async fn finish(self, within: Duration) -> Result<Profile, AccountError> {
        let a = &self.account;
        let tokens = self.sign_in.finish(&a.0.http, within).await?;
        let refresh = tokens.refresh.clone().ok_or_else(|| {
            AccountError::Service("the account service gave no refresh token".into())
        })?;
        let expires = tokens
            .expires_at
            .unwrap_or_else(|| SystemTime::now() + Duration::from_secs(300));
        a.0.state.lock().unwrap().access = Some((tokens.access.clone(), expires));
        let profile = a.fetch_profile(&tokens.access).await?;
        let before = a.saved()?;
        a.save(&Saved {
            refresh_token: refresh,
            profile: profile.clone(),
        })?;
        a.0.state.lock().unwrap().profile = Some(profile.clone());
        a.changed();
        if let Some(old) = before {
            a.revoke(&old.refresh_token).await;
        }
        Ok(profile)
    }
}
