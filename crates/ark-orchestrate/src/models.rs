//! From an agent's model spec to the models it runs on. The orchestrator asks a
//! [`ModelResolver`] for an agent's [`AgentModels`] and never looks at the vendor:
//!
//! ```text
//! agent "researcher", model "claude:sonnet"
//!   -> Models::resolve(lane, spec)
//!   -> SubscriptionRuntime::session(lane, spec)   one managed vendor session per agent
//!   -> CliModel (Claude Code)                      opened, resumed, rotated, released
//! ```
//!
//! [`Models`] is the default resolver. A subscription spec (`claude:*`, `codex:*`) gets a
//! [`ManagedSession`] of its own for reasoning and a stateless call on the same account for
//! the planner and critic; any other spec gets the provider `ark_model::build` makes. Every
//! model sits behind its account's [`Limiter`], so the scheduler and each call respect the
//! account's capacity.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use ark_model::capacity::{Capacity, CapacitySnapshot, Limited, Limiter};
use ark_model::session::{SessionLedger, SubscriptionRuntime};
use ark_model::{ManagedSession, Provider, SessionPolicy, SessionSnapshot};
use ark_runtime::router::CurrentModel;
pub use ark_runtime::StoreLedger;
use ark_runtime::{ModelRouter, Role};

use crate::OrchError;

/// The models of one agent.
pub struct AgentModels {
    pub router: ModelRouter,
    /// The account whose capacity it draws on (`claude`, `ollama`, ...).
    pub account: String,
    pub capacity: Capacity,
    /// The model it runs on, as a spec (`claude:sonnet`).
    pub model: String,
    /// Its managed vendor session, when the model is a subscription.
    pub session: Option<Arc<ManagedSession>>,
}

impl AgentModels {
    pub(crate) fn connections(&self) -> [(String, u64); 2] {
        [Role::Plan, Role::Reason].map(|role| {
            let provider = self.router.provider(role);
            (provider.model_id(), provider.connection_revision())
        })
    }

    pub(crate) fn check_guard(
        &self,
        guard: &dyn ark_runtime::guard::CallGuard,
    ) -> Result<(), OrchError> {
        self.require_host_tools()?;
        for (model, revision) in self.connections() {
            guard
                .check_model(&model, revision)
                .map_err(OrchError::NotAllowed)?;
        }
        Ok(())
    }
    pub(crate) fn require_host_tools(&self) -> Result<(), OrchError> {
        for role in [Role::Plan, Role::Reason] {
            if !self.router.provider(role).host_controls_tools() {
                return Err(OrchError::NotAllowed("the selected model cannot verify that all actions pass through this execution's approvals".into()));
            }
        }
        Ok(())
    }
}

/// Resolves agents' model specs. Implement it to route agents your own way.
#[async_trait::async_trait]
pub trait ModelResolver: Send + Sync {
    /// The models for the agent on `lane` (unique per agent), asking for `spec` (`None`: the
    /// default model).
    async fn resolve(&self, lane: &str, spec: Option<&str>) -> Result<AgentModels, OrchError>;
    /// The execution spec captured for a colleague created by this turn.
    fn selection_spec(&self, models: &AgentModels) -> String {
        models.model.clone()
    }
    /// Validate a new explicit selection without invoking a model. Custom resolvers must
    /// implement their own admission policy before exposing explicit creation choices.
    async fn validate_selection(&self, _spec: &str) -> Result<(), OrchError> {
        Err(OrchError::NotAllowed(
            "explicit model selection is not supported by this resolver".into(),
        ))
    }
    /// Changes whenever the default model changes. The orchestrator resolves an agent that has
    /// no model of its own again before its next task when this moved since it was resolved,
    /// so the agent follows the session's model. A resolver whose default never changes
    /// keeps the default 0.
    fn default_version(&self) -> u64 {
        0
    }
    /// Whether cached models still use the account connection that was resolved.
    fn is_current(&self, _models: &AgentModels) -> bool {
        true
    }
    /// The capacity of an account (for scheduling).
    fn capacity_of(&self, account: &str) -> Capacity {
        Capacity::for_provider(account)
    }
    /// Every account's limiter, for display.
    fn capacity(&self) -> Vec<CapacitySnapshot> {
        Vec::new()
    }
    /// Every managed session, for display.
    fn sessions(&self) -> Vec<SessionSnapshot> {
        Vec::new()
    }
    /// A lane's stored session state, for an agent not resolved yet in this process.
    fn stored_session(&self, _lane: &str) -> Option<SessionSnapshot> {
        None
    }
    /// The agent on `lane` is gone: release its session.
    async fn close(&self, _lane: &str) {}
    /// Release idle sessions. Returns how many.
    async fn sweep_idle(&self) -> usize {
        0
    }
}

/// The default resolver.
pub struct Models {
    subs: SubscriptionRuntime,
    default: Option<CurrentModel>,
    default_spec: Option<String>,
    named: HashMap<String, Arc<dyn Provider>>,
    capacities: HashMap<String, Capacity>,
    limiters: Mutex<HashMap<String, Arc<Limiter>>>,
}

/// Builder for [`Models`].
#[derive(Default)]
pub struct ModelsBuilder {
    default: Option<CurrentModel>,
    default_spec: Option<String>,
    named: HashMap<String, Arc<dyn Provider>>,
    capacities: HashMap<String, Capacity>,
    policy: Option<SessionPolicy>,
    ledger: Option<Arc<dyn SessionLedger>>,
    subs: Option<SubscriptionRuntime>,
}

impl ModelsBuilder {
    /// The router agents without a model spec use, fixed. To follow the app's model as it
    /// changes, use [`follow`](Self::follow).
    pub fn default_router(mut self, r: ModelRouter) -> Self {
        self.default = Some(CurrentModel::new(r));
        self
    }
    /// Agents without a model spec run on `current` as it is when they are resolved (the
    /// orchestrator passes its `Ark`'s [`current_model`](ark_sdk::Ark::current_model)).
    pub fn follow(mut self, current: CurrentModel) -> Self {
        self.default = Some(current);
        self
    }
    /// A spec agents without one use, instead of a router (`claude:sonnet`).
    pub fn default_model(mut self, spec: impl Into<String>) -> Self {
        self.default_spec = Some(spec.into());
        self
    }
    /// A provider an agent can name as its model (`"mock-a"`, `"local-small"`). It is used
    /// for every role, behind a limiter for the account `name`.
    pub fn provider(mut self, name: impl Into<String>, p: Arc<dyn Provider>) -> Self {
        self.named.insert(name.into(), p);
        self
    }
    /// Limits for an account (default: [`Capacity::for_provider`]).
    pub fn capacity(mut self, account: impl Into<String>, c: Capacity) -> Self {
        self.capacities.insert(account.into(), c);
        self
    }
    /// When managed sessions are reused, rotated and released (default:
    /// [`SessionPolicy::from_env`]).
    pub fn session_policy(mut self, p: SessionPolicy) -> Self {
        self.policy = Some(p);
        self
    }
    /// Where vendor session state survives a restart (the orchestrator passes a store
    /// ledger).
    pub fn ledger(mut self, l: Arc<dyn SessionLedger>) -> Self {
        self.ledger = Some(l);
        self
    }
    /// Use this subscription runtime (its own backends, ledger and policy) instead of building
    /// one. The orchestrator passes the `Ark`'s, so single-agent turns and agents share each
    /// account's capacity. Capacities set here still apply if the account is not in use yet.
    pub fn subscriptions(mut self, s: SubscriptionRuntime) -> Self {
        self.subs = Some(s);
        self
    }
    pub fn build(self) -> Models {
        if let Some(subs) = &self.subs {
            for (account, c) in &self.capacities {
                if subs.manages(account) && !subs.set_capacity(account, *c) {
                    tracing::warn!(%account, "the account is already in use; its capacity stays as it was");
                }
            }
        }
        let subs = self.subs.unwrap_or_else(|| {
            let mut b = SubscriptionRuntime::builder();
            if let Some(p) = self.policy {
                b = b.policy(p);
            }
            if let Some(l) = self.ledger {
                b = b.ledger(l);
            }
            for (a, c) in &self.capacities {
                b = b.capacity(a.clone(), *c);
            }
            b.build()
        });
        Models {
            subs,
            default: self.default,
            default_spec: self.default_spec,
            named: self.named,
            capacities: self.capacities,
            limiters: Mutex::new(HashMap::new()),
        }
    }
}

impl Models {
    pub fn builder() -> ModelsBuilder {
        ModelsBuilder::default()
    }

    pub fn subscriptions(&self) -> &SubscriptionRuntime {
        &self.subs
    }

    fn limiter(&self, account: &str) -> Arc<Limiter> {
        if self.subs.manages(account) {
            return self.subs.limiter(account);
        }
        self.limiters
            .lock()
            .unwrap()
            .entry(account.to_string())
            .or_insert_with(|| Arc::new(Limiter::new(account, self.capacity_of(account))))
            .clone()
    }

    fn limited(&self, p: Arc<dyn Provider>, account: &str) -> Arc<dyn Provider> {
        Arc::new(Limited::new(p, self.limiter(account)))
    }

    fn single(&self, p: Arc<dyn Provider>, account: &str, model: String) -> AgentModels {
        let limited = self.limited(p, account);
        AgentModels {
            router: ModelRouter::single(limited),
            account: account.to_string(),
            capacity: self.capacity_of(account),
            model,
            session: None,
        }
    }
}

/// The account of a model id: the provider part (`claude` of `claude:sonnet`).
pub fn account_of(model_id: &str) -> String {
    model_id
        .split([':', ' '])
        .next()
        .unwrap_or(model_id)
        .to_string()
}

#[async_trait::async_trait]
impl ModelResolver for Models {
    fn selection_spec(&self, models: &AgentModels) -> String {
        if self.named.contains_key(&models.model) {
            models.model.clone()
        } else {
            models
                .router
                .provider(Role::Reason)
                .describe()
                .replacen(' ', ":", 1)
        }
    }

    async fn validate_selection(&self, spec: &str) -> Result<(), OrchError> {
        if self.named.contains_key(spec) {
            return Ok(());
        }
        let catalog = ark_model::catalog().await;
        validate_catalog_selection(&catalog, spec)
    }

    async fn resolve(&self, lane: &str, spec: Option<&str>) -> Result<AgentModels, OrchError> {
        let captured = self
            .default
            .as_ref()
            .map(CurrentModel::get)
            .filter(|router| {
                spec.is_some_and(|s| {
                    !self.named.contains_key(s)
                        && (router.model_id(Role::Reason) == s
                            || router
                                .provider(Role::Reason)
                                .describe()
                                .replacen(' ', ":", 1)
                                == s)
                })
            });
        let selected = if captured.is_some() {
            None
        } else {
            spec.or(self.default_spec.as_deref())
        };
        let spec = match selected {
            Some(s) => s.to_string(),
            None => match captured.or_else(|| self.default.as_ref().map(CurrentModel::get)) {
                Some(r) => {
                    let r = r.for_conversation(&format!("agent/{lane}"))?;
                    let id = r.model_id(Role::Reason);
                    // The app's own model is a subscription CLI: the agent still gets a
                    // managed session of its own. Decided by the provider (`claude sonnet`),
                    // not the model id: an endpoint's model may be called `claude`.
                    let spec = r.provider(Role::Reason).describe().replacen(' ', ":", 1);
                    if self.subs.manages(&spec) {
                        id
                    } else {
                        let account = account_of(&id);
                        let router = ModelRouter::new(
                            self.limited(r.provider(Role::Plan).clone(), &account),
                            self.limited(r.provider(Role::Reason).clone(), &account),
                        );
                        return Ok(AgentModels {
                            router,
                            capacity: self.capacity_of(&account),
                            account,
                            model: id,
                            session: None,
                        });
                    }
                }
                None => {
                    return Err(OrchError::NotAllowed(
                        "no model: set a default model or give the agent one".into(),
                    ))
                }
            },
        };
        if let Some(p) = self.named.get(&spec) {
            let p = p
                .for_conversation(&format!("agent/{lane}"))?
                .unwrap_or_else(|| p.clone());
            return Ok(self.single(p, &spec, spec.clone()));
        }
        if let Some(session) = self.subs.session(lane, &spec).await {
            let (provider, _) = ark_model::session::split_spec(&spec);
            let account = provider.to_string();
            let stateless = self
                .subs
                .stateless(&spec)
                .expect("a spec with a session has a stateless form");
            let router = ModelRouter::new(stateless, session.clone() as Arc<dyn Provider>);
            return Ok(AgentModels {
                router,
                capacity: self.capacity_of(&account),
                account,
                model: session.model_id(),
                session: Some(session),
            });
        }
        let (provider, model) = ark_model::session::split_spec(&spec);
        let p = ark_model::build(provider, (!model.is_empty()).then(|| model.to_string()))?;
        let p = p.for_conversation(&format!("agent/{lane}"))?.unwrap_or(p);
        let model = p.model_id();
        Ok(self.single(p, provider, model))
    }

    fn is_current(&self, models: &AgentModels) -> bool {
        if !models.account.contains('~') {
            return true;
        }
        let revision = models.router.provider(Role::Reason).connection_revision();
        ark_model::accounts::get(&models.account).is_ok_and(|account| {
            account.is_some_and(|a| {
                a.state == ark_model::accounts::AccountState::Active && a.revision == revision
            })
        })
    }

    fn default_version(&self) -> u64 {
        match (&self.default_spec, &self.default) {
            (None, Some(c)) => c.version(),
            _ => 0,
        }
    }

    fn capacity_of(&self, account: &str) -> Capacity {
        self.capacities.get(account).copied().unwrap_or_else(|| {
            if self.subs.manages(account) {
                self.subs.limiter(account).capacity()
            } else {
                Capacity::for_provider(account)
            }
        })
    }

    fn capacity(&self) -> Vec<CapacitySnapshot> {
        let mut v = self.subs.capacity();
        v.extend(self.limiters.lock().unwrap().values().map(|l| l.snapshot()));
        v.sort_by(|a, b| a.account.cmp(&b.account));
        v
    }

    fn sessions(&self) -> Vec<SessionSnapshot> {
        self.subs.sessions()
    }

    fn stored_session(&self, lane: &str) -> Option<SessionSnapshot> {
        self.subs.stored(lane)
    }

    async fn close(&self, lane: &str) {
        self.subs.close(lane).await;
    }

    async fn sweep_idle(&self) -> usize {
        self.subs.sweep_idle().await
    }
}

fn validate_catalog_selection(catalog: &[ark_model::Backend], spec: &str) -> Result<(), OrchError> {
    let accepted = spec.split_once(':').is_some_and(|(provider, selected)| {
        let (model, effort) = selected
            .split_once('@')
            .map_or((selected, None), |(m, e)| (m, Some(e)));
        catalog.iter().any(|backend| {
            backend.provider == provider
                && backend.ready
                && backend.models.iter().any(|id| id == model)
                && effort.is_none_or(|effort| {
                    backend.model_options.iter().any(|option| {
                        option.id == model && option.efforts.iter().any(|e| e == effort)
                    })
                })
        })
    });
    if accepted {
        Ok(())
    } else {
        Err(OrchError::NotAllowed(
            "that model, account or reasoning effort is unavailable".into(),
        ))
    }
}

#[cfg(test)]
mod selection_tests {
    use super::*;
    #[test]
    fn creation_catalog_rejects_missing_accounts_models_and_unsupported_effort() {
        let catalog = vec![ark_model::Backend {
            provider: "provider-a~one".into(),
            label: "Codex".into(),
            identity: None,
            kind: ark_model::BackendKind::Subscription,
            ready: true,
            detail: String::new(),
            fix: None,
            models: vec!["large".into()],
            model_options: vec![ark_model::ModelOption {
                id: "large".into(),
                label: "GPT".into(),
                efforts: vec!["low".into(), "high".into()],
                default_effort: Some("low".into()),
            }],
            catalog_state: Some(ark_model::CatalogState::Current),
            step: None,
        }];
        for spec in ["provider-a~one:large", "provider-a~one:large@high"] {
            assert!(validate_catalog_selection(&catalog, spec).is_ok());
        }
        for spec in [
            "provider-a:large",
            "provider-a~two:large",
            "provider-a~one:retired",
            "provider-a~one:large@max",
            "provider-a~one:large@",
        ] {
            assert!(
                validate_catalog_selection(&catalog, spec).is_err(),
                "{spec}"
            );
        }
        let mut offline = catalog;
        offline[0].ready = false;
        assert!(validate_catalog_selection(&offline, "provider-a~one:large").is_err());
    }
}
