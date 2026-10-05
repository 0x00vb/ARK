//! API keys for the hosted model APIs. The process environment comes first
//! (`ANTHROPIC_API_KEY`); then the app's key store, installed once with [`set_source`]
//! (ark-connect keeps saved keys in the OS keychain). So an app never puts a secret into the
//! process environment, and a key saved while the app runs is used by the next request.

use std::sync::{Arc, RwLock};

/// Where saved keys are read from.
pub trait KeySource: Send + Sync {
    /// The key for `var` (`ANTHROPIC_API_KEY`), when one is saved. Reading may ask the person
    /// (a keychain prompt), so it happens only when a key is about to be used.
    fn key(&self, var: &str) -> Option<String>;
    /// Whether a key is saved for `var`, without reading it.
    fn has(&self, var: &str) -> bool;
}

static SOURCE: RwLock<Option<Arc<dyn KeySource>>> = RwLock::new(None);

/// Read saved keys from `source` from now on, for the whole process.
pub fn set_source(source: Arc<dyn KeySource>) {
    *SOURCE.write().unwrap() = Some(source);
}

fn env(var: &str) -> Option<String> {
    std::env::var(var).ok().filter(|k| !k.trim().is_empty())
}

/// The key for `var`: the environment's, else a saved one.
pub fn api_key(var: &str) -> Option<String> {
    env(var).or_else(|| SOURCE.read().unwrap().as_ref()?.key(var))
}

/// Whether a key for `var` is there (in the environment or saved), without reading a saved one.
pub fn has_api_key(var: &str) -> bool {
    env(var).is_some() || SOURCE.read().unwrap().as_ref().is_some_and(|s| s.has(var))
}
