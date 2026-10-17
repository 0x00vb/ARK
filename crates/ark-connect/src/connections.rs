//! The accounts ARK is connected to, and the capabilities they make ready. Account records
//! (provider, who, scopes) are in `<root>/connections.json`; refresh tokens are in the
//! [`Secrets`] store; access tokens only in memory.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use ark_tools::{Adapter, ToolSource};
use serde::{Deserialize, Serialize};

use crate::catalog::{Capability, Catalog, Provider};
use crate::oauth::{self, SignIn, Tokens};
use crate::secrets::Secrets;
use crate::services;
use crate::ConnectError;

const FILE: &str = "connections.json";

/// One signed-in account.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Account {
    /// `<provider>:<who>` (`google:me@example.com`).
    pub id: String,
    pub provider: String,
    /// Who it is, as the provider says (an email address).
    pub name: String,
    /// The scopes granted so far.
    pub scopes: Vec<String>,
    /// Unix seconds.
    pub connected_at: i64,
}

/// A capability as a front-end lists it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CapabilityStatus {
    pub id: String,
    pub label: String,
    pub says: String,
    #[serde(flatten)]
    pub state: State,
    /// The provider's label (`Google`), for a capability that needs an account.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// The account it uses, when connected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "state", content = "why", rename_all = "snake_case")]
pub enum State {
    /// Agents can use it now.
    Ready,
    /// One sign-in away.
    Connect,
    /// The app offers it; the person turns it on (the Computer, ADR 0045).
    Off,
    /// This build cannot connect it, and why.
    Unavailable(String),
}

#[derive(Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    accounts: Vec<Account>,
}

struct Inner {
    root: PathBuf,
    catalog: Catalog,
    secrets: Arc<dyn Secrets>,
    http: reqwest::Client,
    accounts: RwLock<Vec<Account>>,
    /// Access tokens by account id.
    tokens: Mutex<HashMap<String, Tokens>>,
    /// Service base URLs that replace the real ones (tests, a proxy).
    bases: HashMap<String, String>,
    api_keys: crate::ApiKeys,
}

/// The engine's connected accounts and what they make ready. Cheap to clone.
#[derive(Clone)]
pub struct Connections {
    inner: Arc<Inner>,
}

/// A connected account that delivers events ([`Connections::event_sources`]).
pub struct EventFeed {
    /// In words: `Inbox (office@example.com)`.
    pub label: String,
    /// The actions its capability gives, which a run on its events may be allowed.
    pub actions: Vec<String>,
    pub source: Arc<dyn ark_tools::events::EventSource>,
}

/// A connection in progress: send the person to [`url`](Connecting::url), then
/// [`finish`](Connecting::finish).
pub struct Connecting {
    sign_in: SignIn,
    provider: Provider,
    /// The scopes asked for, for a provider whose reply does not list what it granted.
    asked: Vec<String>,
    conns: Connections,
}

impl Connections {
    /// Read `<root>/connections.json`. A missing or unreadable file is no accounts.
    pub fn open(root: &Path, catalog: Catalog, secrets: Arc<dyn Secrets>) -> Self {
        let api_keys = crate::ApiKeys::open(root, secrets.clone());
        let file: File = std::fs::read_to_string(root.join(FILE))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        Connections {
            inner: Arc::new(Inner {
                root: root.to_path_buf(),
                catalog,
                secrets,
                http: reqwest::Client::builder()
                    .timeout(Duration::from_secs(30))
                    .build()
                    .unwrap_or_default(),
                accounts: RwLock::new(file.accounts),
                tokens: Mutex::new(HashMap::new()),
                bases: HashMap::new(),
                api_keys,
            }),
        }
    }

    /// Saved API keys for the hosted model APIs, in the same secret store.
    pub fn api_keys(&self) -> &crate::ApiKeys {
        &self.inner.api_keys
    }

    /// Send `service`'s requests to `base` instead (tests, a proxy). Before the first clone.
    pub fn with_base(mut self, service: &str, base: &str) -> Self {
        if let Some(inner) = Arc::get_mut(&mut self.inner) {
            inner.bases.insert(service.into(), base.into());
        }
        self
    }

    pub fn catalog(&self) -> &Catalog {
        &self.inner.catalog
    }

    pub fn accounts(&self) -> Vec<Account> {
        self.inner.accounts.read().unwrap().clone()
    }

    /// The account a capability would use: one of its provider's with every scope it needs.
    pub fn account_for(&self, cap: &Capability) -> Option<Account> {
        let provider = cap.provider.as_deref()?;
        self.accounts()
            .into_iter()
            .find(|a| a.provider == provider && cap.scopes.iter().all(|s| a.scopes.contains(s)))
    }

    /// Every capability of the catalog this app can show. `tools`: the action namespaces the
    /// app already has; a capability that needs no account is listed only when they include
    /// its tools.
    pub fn status(&self, tools: &[String]) -> Vec<CapabilityStatus> {
        let cat = &self.inner.catalog;
        cat.capabilities
            .iter()
            .filter_map(|c| {
                let provider = c.provider.as_deref().and_then(|p| cat.provider(p));
                let (state, account) = match (&c.provider, provider) {
                    (None, _) => {
                        if !c.tools.iter().any(|t| tools.contains(t)) {
                            return None;
                        }
                        (State::Ready, None)
                    }
                    (Some(p), None) => {
                        (State::Unavailable(format!("unknown provider `{p}`")), None)
                    }
                    (Some(_), Some(p)) => match self.account_for(c) {
                        Some(a) => (State::Ready, Some(a.name)),
                        None if p.oauth.client_id().is_some() => (State::Connect, None),
                        None => (
                            State::Unavailable(format!(
                                "this build of ARK cannot sign in to {} yet",
                                p.label
                            )),
                            None,
                        ),
                    },
                };
                Some(CapabilityStatus {
                    id: c.id.clone(),
                    label: c.label.clone(),
                    says: c.says.clone(),
                    state,
                    provider: provider.map(|p| p.label.clone()),
                    account,
                })
            })
            .collect()
    }

    /// Start connecting `capability`: a sign-in for its scopes, added to what an account of
    /// the same provider already has.
    pub async fn begin(&self, capability: &str) -> Result<Connecting, ConnectError> {
        let cat = &self.inner.catalog;
        let cap = cat
            .capability(capability)
            .ok_or_else(|| ConnectError::Unknown(capability.into()))?;
        let pid = cap
            .provider
            .as_deref()
            .ok_or_else(|| ConnectError::NotSetUp(format!("{} needs no account", cap.label)))?;
        let provider = cat
            .provider(pid)
            .ok_or_else(|| ConnectError::NotSetUp(format!("unknown provider `{pid}`")))?
            .clone();
        let mut scopes: Vec<String> = self
            .accounts()
            .into_iter()
            .filter(|a| a.provider == pid)
            .flat_map(|a| a.scopes)
            .collect();
        for s in &cap.scopes {
            if !scopes.contains(s) {
                scopes.push(s.clone());
            }
        }
        let sign_in = oauth::begin(&provider, &scopes).await?;
        Ok(Connecting {
            sign_in,
            provider,
            asked: scopes,
            conns: self.clone(),
        })
    }

    /// Disconnect an account (its id, or a capability it serves): revoke at the provider,
    /// delete its token, forget it. Every capability it served needs connecting again.
    pub async fn disconnect(&self, which: &str) -> Result<Account, ConnectError> {
        let account = self
            .accounts()
            .into_iter()
            .find(|a| a.id == which || a.provider == which)
            .or_else(|| {
                self.inner
                    .catalog
                    .capability(which)
                    .and_then(|c| self.account_for(c))
            })
            .ok_or_else(|| ConnectError::NotConnected(format!("`{which}` is not connected")))?;
        let key = secret_key(&account.id);
        if let (Some(provider), Some(token)) = (
            self.inner.catalog.provider(&account.provider),
            self.inner.secrets.get(&key)?,
        ) {
            if let Err(e) = oauth::revoke(&self.inner.http, provider, &token).await {
                tracing::warn!("revoke {}: {e}", account.id);
            }
        }
        self.inner.secrets.delete(&key)?;
        self.inner.tokens.lock().unwrap().remove(&account.id);
        self.inner
            .accounts
            .write()
            .unwrap()
            .retain(|a| a.id != account.id);
        self.save()?;
        Ok(account)
    }

    /// A usable access token for an account: the one in memory while fresh, else a new one
    /// from its refresh token.
    pub async fn access_token(&self, account: &str) -> Result<String, ConnectError> {
        if let Some(t) = self.inner.tokens.lock().unwrap().get(account) {
            if t.fresh() {
                return Ok(t.access.clone());
            }
        }
        let rec = self
            .accounts()
            .into_iter()
            .find(|a| a.id == account)
            .ok_or_else(|| ConnectError::NotConnected(format!("{account} is not connected")))?;
        let provider = self.inner.catalog.provider(&rec.provider).ok_or_else(|| {
            ConnectError::NotSetUp(format!("unknown provider `{}`", rec.provider))
        })?;
        let refresh = self
            .inner
            .secrets
            .get(&secret_key(account))?
            .ok_or_else(|| {
                ConnectError::NotConnected(format!(
                    "{}'s sign-in is gone from the keychain; connect again",
                    rec.name
                ))
            })?;
        let tokens = oauth::refresh(&self.inner.http, provider, &refresh)
            .await
            .map_err(|e| {
                ConnectError::NotConnected(format!("{} needs connecting again: {e}", rec.name))
            })?;
        if let Some(new) = &tokens.refresh {
            self.inner.secrets.set(&secret_key(account), new)?;
        }
        let access = tokens.access.clone();
        self.inner
            .tokens
            .lock()
            .unwrap()
            .insert(account.to_string(), tokens);
        Ok(access)
    }

    /// Forget an account's access token (the service refused it).
    pub fn expire(&self, account: &str) {
        self.inner.tokens.lock().unwrap().remove(account);
    }

    /// The capabilities one sign-in away now: they need an account this app can sign in to,
    /// and none is connected.
    pub fn connectable(&self) -> Vec<&Capability> {
        let cat = &self.inner.catalog;
        cat.capabilities
            .iter()
            .filter(|c| {
                c.provider
                    .as_deref()
                    .and_then(|p| cat.provider(p))
                    .is_some_and(|p| p.oauth.client_id().is_some())
                    && self.account_for(c).is_none()
            })
            .collect()
    }

    /// The tools a capability gives once connected. `None`: not connected, or no service.
    pub fn tools_of(&self, cap: &Capability) -> Option<Arc<dyn Adapter>> {
        let account = self.account_for(cap)?;
        services::adapter(cap.service.as_deref()?, self.clone(), account.id)
    }

    /// The connected accounts that deliver events, each with the poller of its events.
    pub fn event_sources(&self) -> Vec<EventFeed> {
        self.inner
            .catalog
            .capabilities
            .iter()
            .filter_map(|c| {
                let service = c.service.as_deref()?;
                let account = self.account_for(c)?;
                let source = services::feed(
                    service,
                    self.clone(),
                    account.id.clone(),
                    account.name.clone(),
                )?;
                let actions = self
                    .tools_of(c)
                    .map(|t| t.actions().into_iter().map(|d| d.name).collect())
                    .unwrap_or_default();
                Some(EventFeed {
                    label: format!("{} ({})", c.label, account.name),
                    actions,
                    source,
                })
            })
            .collect()
    }

    pub(crate) fn http(&self) -> &reqwest::Client {
        &self.inner.http
    }

    pub(crate) fn base(&self, service: &str) -> Option<&str> {
        self.inner.bases.get(service).map(String::as_str)
    }

    fn save(&self) -> Result<(), ConnectError> {
        let file = File {
            accounts: self.accounts(),
        };
        let text = serde_json::to_string_pretty(&file).expect("accounts serialize");
        let path = self.inner.root.join(FILE);
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text)?;
        ark_os::fs::replace(&tmp, &path)?;
        Ok(())
    }
}

impl Connecting {
    /// The page the person signs in on, in their own browser.
    pub fn url(&self) -> &str {
        self.sign_in.url()
    }

    /// Wait for the sign-in (at most `within`) and keep the account.
    pub async fn finish(self, within: Duration) -> Result<Account, ConnectError> {
        let conns = self.conns;
        let tokens = self.sign_in.finish(conns.http(), within).await?;
        let name = tokens
            .id_token
            .as_deref()
            .and_then(|t| oauth::claim(t, "email").or_else(|| oauth::claim(t, "sub")))
            .ok_or_else(|| {
                ConnectError::Provider(format!("{} did not say which account", self.provider.label))
            })?;
        let id = format!("{}:{name}", self.provider.id);
        let mut accounts = conns.accounts();
        let old = accounts.iter().position(|a| a.id == id);
        let mut scopes = old.map(|i| accounts[i].scopes.clone()).unwrap_or_default();
        let granted = if tokens.scopes.is_empty() {
            &self.asked
        } else {
            &tokens.scopes
        };
        for s in granted {
            if !scopes.contains(s) {
                scopes.push(s.clone());
            }
        }
        let key = secret_key(&id);
        match &tokens.refresh {
            Some(r) => conns.inner.secrets.set(&key, r)?,
            None if conns.inner.secrets.get(&key)?.is_some() => {}
            None => {
                return Err(ConnectError::Provider(format!(
                    "{} gave no lasting sign-in; try again",
                    self.provider.label
                )))
            }
        }
        let account = Account {
            id: id.clone(),
            provider: self.provider.id.clone(),
            name,
            scopes,
            connected_at: time::OffsetDateTime::now_utc().unix_timestamp(),
        };
        match old {
            Some(i) => accounts[i] = account.clone(),
            None => accounts.push(account.clone()),
        }
        *conns.inner.accounts.write().unwrap() = accounts;
        conns.inner.tokens.lock().unwrap().insert(id, tokens);
        conns.save()?;
        Ok(account)
    }
}

/// The connected capabilities' tools, as they are when a turn starts.
impl ToolSource for Connections {
    fn adapters(&self) -> Vec<Arc<dyn Adapter>> {
        self.inner
            .catalog
            .capabilities
            .iter()
            .filter_map(|c| self.tools_of(c))
            .collect()
    }
}

fn secret_key(account: &str) -> String {
    format!("account:{account}")
}
