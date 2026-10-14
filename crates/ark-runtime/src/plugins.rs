//! MCP plugins that come and go while the engine runs. `<root>/plugins.json` lists them; the
//! engine connects each at start and can add, remove or restart one later. Each server's tools
//! become actions named `<plugin>.<tool>` ([`ark_tools::McpAdapter`]).
//!
//! A change applies from the next turn or task: a turn's tools are fixed when it starts
//! ([`Engine::tools`](crate::Engine::tools)), so a running turn never sees its tool list move.
//!
//! Adding a plugin starts a process. It is the person's action (a command, a settings page),
//! never an agent's.
//!
//! ```json
//! { "plugins": [ { "name": "notes", "command": "python3", "args": ["notes_server.py"],
//!                  "env": { "NOTES_DIR": "/data" } } ] }
//! ```
//!
//! Relative `command` and `args` paths resolve against the root.

/// Provider-neutral marketplace records. These do not grant execution by themselves.
pub mod catalog;
pub mod provider;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use ark_tools::{Adapter, McpAdapter};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;

use crate::error::AgentError;

const FILE: &str = "plugins.json";
const MARKETPLACE_FILE: &str = "marketplace.json";

/// An inert marketplace installation. It records a person's Add decision only. A provider
/// binding and an approval are separate records and actions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarketplacePlugin {
    pub id: String,
    pub added_at: OffsetDateTime,
}

/// The durable, versioned form of local marketplace choices. A Cloud connection belongs to the
/// person's ARK account. This file only controls what this installation shows in its catalog.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct MarketplaceFile {
    schema_version: u16,
    plugins: Vec<MarketplacePlugin>,
}

impl MarketplaceFile {
    const VERSION: u16 = 1;

    fn empty() -> Self {
        Self {
            schema_version: Self::VERSION,
            plugins: Vec::new(),
        }
    }
}

/// One entry of `plugins.json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PluginSpec {
    /// The actions' prefix: `notes` gives `notes.search`. Lower-case letters, digits, `-`, `_`.
    pub name: String,
    pub command: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Fields this version does not know, kept as they were.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

impl PluginSpec {
    pub fn new(name: impl Into<String>, command: impl Into<String>) -> Self {
        PluginSpec {
            name: name.into(),
            command: command.into(),
            args: Vec::new(),
            env: BTreeMap::new(),
            extra: serde_json::Map::new(),
        }
    }
    pub fn args<S: Into<String>>(mut self, args: impl IntoIterator<Item = S>) -> Self {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }
}

/// A plugin as a front-end shows it. Environment values stay out: they are often keys.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PluginStatus {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env_keys: Vec<String>,
    /// Its server is connected and running.
    pub running: bool,
    /// The actions it offers, `<plugin>.<tool>`.
    pub tools: Vec<String>,
    /// Why it is not running.
    pub error: Option<String>,
}

struct Entry {
    spec: PluginSpec,
    adapter: Option<Arc<McpAdapter>>,
    error: Option<String>,
}

impl Entry {
    fn status(&self) -> PluginStatus {
        let running = self.adapter.as_ref().is_some_and(|a| a.running());
        PluginStatus {
            name: self.spec.name.clone(),
            command: self.spec.command.clone(),
            args: self.spec.args.clone(),
            env_keys: self.spec.env.keys().cloned().collect(),
            running,
            tools: self
                .adapter
                .as_ref()
                .map(|a| a.actions().into_iter().map(|d| d.name).collect())
                .unwrap_or_default(),
            error: if running {
                None
            } else {
                self.error
                    .clone()
                    .or_else(|| Some("the server stopped".into()))
            },
        }
    }
}

impl ark_tools::ToolSource for Plugins {
    fn adapters(&self) -> Vec<Arc<dyn Adapter>> {
        Plugins::adapters(self)
    }
}

/// The engine's plugins: what `plugins.json` lists, each connected or with the reason it is not.
pub struct Plugins {
    root: PathBuf,
    entries: RwLock<Vec<Entry>>,
    marketplace: RwLock<Result<MarketplaceFile, String>>,
    limit: Duration,
}

impl Plugins {
    /// Read `<root>/plugins.json` and connect every plugin. A plugin that fails stays listed
    /// with its error, so a front-end can show why; it never blocks the engine from starting.
    pub fn load(root: &Path) -> Self {
        let plugins = Plugins {
            root: root.to_path_buf(),
            entries: RwLock::new(Vec::new()),
            marketplace: RwLock::new(Ok(MarketplaceFile::empty())),
            limit: ark_tools::mcp::HANDSHAKE_TIMEOUT,
        };
        let specs = match plugins.read_file() {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("{FILE}: {e}");
                Vec::new()
            }
        };
        let entries = specs
            .into_iter()
            .map(|spec| {
                let (adapter, error) = match plugins.connect(&spec) {
                    Ok(a) => {
                        tracing::info!("loaded MCP plugin `{}`", spec.name);
                        (Some(a), None)
                    }
                    Err(e) => {
                        tracing::warn!("MCP plugin `{}` failed to load: {e}", spec.name);
                        (None, Some(e.to_string()))
                    }
                };
                Entry {
                    spec,
                    adapter,
                    error,
                }
            })
            .collect();
        *plugins.entries.write().unwrap() = entries;
        *plugins.marketplace.write().unwrap() = plugins.read_marketplace();
        if let Err(e) = plugins.marketplace.read().unwrap().as_ref() {
            // Leave malformed or newer data intact. Mutations remain disabled until recovery.
            tracing::warn!("{MARKETPLACE_FILE}: {e}");
        }
        plugins
    }

    /// A shorter limit on a server's handshake (tests).
    pub fn with_handshake_limit(mut self, limit: Duration) -> Self {
        self.limit = limit;
        self
    }

    /// Every plugin, in the file's order.
    pub fn list(&self) -> Vec<PluginStatus> {
        self.entries
            .read()
            .unwrap()
            .iter()
            .map(Entry::status)
            .collect()
    }

    /// Marketplace entries the person has added. They do not execute, connect an account or
    /// grant an agent access by themselves.
    pub fn marketplace(&self) -> Vec<MarketplacePlugin> {
        self.marketplace
            .read()
            .unwrap()
            .as_ref()
            .map(|file| file.plugins.clone())
            .unwrap_or_default()
    }

    /// Persist an Add decision. The catalog source and provider connection are deliberately not
    /// inferred here: a front-end must show them and ask for the later Connect action.
    pub fn add_marketplace(&self, id: &str) -> Result<MarketplacePlugin, AgentError> {
        let id = id.trim();
        if !valid_marketplace_id(id) {
            return Err(AgentError::Plugin(
                "a marketplace id has invalid characters".into(),
            ));
        }
        let mut state = self.marketplace.write().unwrap();
        let file = state
            .as_mut()
            .map_err(|e| AgentError::Plugin(format!("cannot change marketplace choices: {e}")))?;
        if let Some(entry) = file.plugins.iter().find(|entry| entry.id == id) {
            return Ok(entry.clone());
        }
        let entry = MarketplacePlugin {
            id: id.into(),
            added_at: OffsetDateTime::now_utc(),
        };
        let mut next = file.clone();
        next.plugins.push(entry.clone());
        self.write_marketplace(&next)?;
        *file = next;
        Ok(entry)
    }

    /// Forget the local marketplace record. A front-end must separately ask whether to revoke a
    /// connected account, because removal defaults to preserving it.
    pub fn remove_marketplace(&self, id: &str) -> Result<(), AgentError> {
        let mut state = self.marketplace.write().unwrap();
        let file = state
            .as_mut()
            .map_err(|e| AgentError::Plugin(format!("cannot change marketplace choices: {e}")))?;
        let mut next = file.clone();
        let before = next.plugins.len();
        next.plugins.retain(|entry| entry.id != id);
        if next.plugins.len() == before {
            return Err(AgentError::Plugin(format!(
                "no marketplace plugin named `{id}`"
            )));
        }
        self.write_marketplace(&next)?;
        *file = next;
        Ok(())
    }

    /// The connected plugins' adapters.
    pub fn adapters(&self) -> Vec<Arc<dyn Adapter>> {
        self.entries
            .read()
            .unwrap()
            .iter()
            .filter_map(|e| e.adapter.clone().map(|a| a as Arc<dyn Adapter>))
            .collect()
    }

    /// Connect a new plugin and, once it answers, save it to `plugins.json`. `taken` holds
    /// action prefixes already in use (the app's own adapters). Blocks while the server starts.
    pub fn add(&self, spec: PluginSpec, taken: &[String]) -> Result<PluginStatus, AgentError> {
        check_name(&spec.name)?;
        if taken.iter().any(|t| t == &spec.name) {
            return Err(AgentError::Plugin(format!(
                "`{}` is already the name of the app's own tools",
                spec.name
            )));
        }
        if self.has(&spec.name) {
            return Err(AgentError::Plugin(format!(
                "there is already a plugin named `{}`",
                spec.name
            )));
        }
        if spec.command.trim().is_empty() {
            return Err(AgentError::Plugin("a plugin needs a command".into()));
        }
        // Connect outside the lock: a slow server must not hold up turns starting meanwhile.
        let adapter = self
            .connect(&spec)
            .map_err(|e| AgentError::Plugin(format!("`{}` did not start: {e}", spec.name)))?;
        let mut entries = self.entries.write().unwrap();
        if entries.iter().any(|e| e.spec.name == spec.name) {
            return Err(AgentError::Plugin(format!(
                "there is already a plugin named `{}`",
                spec.name
            )));
        }
        entries.push(Entry {
            spec,
            adapter: Some(adapter),
            error: None,
        });
        self.write_file(&entries)?;
        Ok(entries.last().unwrap().status())
    }

    /// Stop a plugin and take it out of `plugins.json`.
    pub fn remove(&self, name: &str) -> Result<(), AgentError> {
        let mut entries = self.entries.write().unwrap();
        let before = entries.len();
        entries.retain(|e| e.spec.name != name);
        if entries.len() == before {
            return Err(AgentError::Plugin(format!("no plugin named `{name}`")));
        }
        self.write_file(&entries)
    }

    /// Stop a plugin's server and start it again (after a crash, or a change to what it reads).
    pub fn restart(&self, name: &str) -> Result<PluginStatus, AgentError> {
        let spec = {
            let mut entries = self.entries.write().unwrap();
            let e = entries
                .iter_mut()
                .find(|e| e.spec.name == name)
                .ok_or_else(|| AgentError::Plugin(format!("no plugin named `{name}`")))?;
            e.adapter = None;
            e.spec.clone()
        };
        let result = self.connect(&spec);
        let mut entries = self.entries.write().unwrap();
        let e = entries
            .iter_mut()
            .find(|e| e.spec.name == name)
            .ok_or_else(|| AgentError::Plugin(format!("no plugin named `{name}`")))?;
        match result {
            Ok(a) => {
                e.adapter = Some(a);
                e.error = None;
            }
            Err(err) => e.error = Some(err.to_string()),
        }
        Ok(e.status())
    }

    fn has(&self, name: &str) -> bool {
        self.entries
            .read()
            .unwrap()
            .iter()
            .any(|e| e.spec.name == name)
    }

    fn connect(&self, spec: &PluginSpec) -> ark_tools::Result<Arc<McpAdapter>> {
        let command = self.resolve(&spec.command, true);
        let args: Vec<String> = spec.args.iter().map(|a| self.resolve(a, false)).collect();
        let env: HashMap<String, String> = spec
            .env
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        McpAdapter::connect_within(&spec.name, &command, &args, &env, self.limit).map(Arc::new)
    }

    /// A relative path that names a file under the root resolves against it; anything else
    /// (`python3`, `--flag`, a word) is passed on as written.
    fn resolve(&self, s: &str, command: bool) -> String {
        let p = Path::new(s);
        let looks_like_path = s.contains('/') || (!command && s.contains('.'));
        if p.is_relative() && looks_like_path && self.root.join(p).exists() {
            self.root.join(p).display().to_string()
        } else {
            s.to_string()
        }
    }

    fn read_file(&self) -> Result<Vec<PluginSpec>, String> {
        let text = match std::fs::read_to_string(self.root.join(FILE)) {
            Ok(t) => t,
            Err(_) => return Ok(Vec::new()), // no file: no plugins
        };
        let doc: Value = serde_json::from_str(&text).map_err(|e| format!("not valid JSON: {e}"))?;
        let mut out = Vec::new();
        for p in doc
            .get("plugins")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            match serde_json::from_value::<PluginSpec>(p) {
                Ok(s) if !s.name.is_empty() && !s.command.is_empty() => out.push(s),
                _ => tracing::warn!("{FILE}: an entry without a name or command was skipped"),
            }
        }
        Ok(out)
    }

    /// Write the list back, keeping the file's other top-level fields.
    fn write_file(&self, entries: &[Entry]) -> Result<(), AgentError> {
        let path = self.root.join(FILE);
        let mut doc: serde_json::Map<String, Value> = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        let specs: Vec<&PluginSpec> = entries.iter().map(|e| &e.spec).collect();
        doc.insert("plugins".into(), serde_json::to_value(specs)?);
        let text = serde_json::to_string_pretty(&Value::Object(doc))?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text + "\n")?;
        ark_os::fs::replace(&tmp, &path)?;
        Ok(())
    }

    fn read_marketplace(&self) -> Result<MarketplaceFile, String> {
        let path = self.root.join(MARKETPLACE_FILE);
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(MarketplaceFile::empty())
            }
            Err(e) => return Err(e.to_string()),
        };
        let file: MarketplaceFile =
            serde_json::from_str(&text).map_err(|e| format!("not valid JSON: {e}"))?;
        if file.schema_version != MarketplaceFile::VERSION {
            return Err(format!(
                "unsupported schema version {}",
                file.schema_version
            ));
        }
        Ok(file)
    }

    fn write_marketplace(&self, file: &MarketplaceFile) -> Result<(), AgentError> {
        let path = self.root.join(MARKETPLACE_FILE);
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(file)? + "\n")?;
        ark_os::fs::replace(&tmp, &path)?;
        Ok(())
    }
}

fn check_name(name: &str) -> Result<(), AgentError> {
    let ok = !name.is_empty()
        && name.len() <= 40
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if ok {
        Ok(())
    } else {
        Err(AgentError::Plugin(format!(
            "`{name}` is not a plugin name (lower-case letters, digits, - and _)"
        )))
    }
}

fn valid_marketplace_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 120
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.')
}

#[cfg(test)]
mod marketplace_tests {
    use super::*;

    #[test]
    fn marketplace_add_is_inert_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let plugins = Plugins::load(dir.path());
        let added = plugins.add_marketplace("composio.gmail").unwrap();
        assert_eq!(added.id, "composio.gmail");
        assert_eq!(plugins.marketplace().len(), 1);
        // Add is idempotent and does not create a process entry.
        plugins.add_marketplace("composio.gmail").unwrap();
        assert!(plugins.list().is_empty());
        let reloaded = Plugins::load(dir.path());
        assert_eq!(reloaded.marketplace(), plugins.marketplace());
        reloaded.remove_marketplace("composio.gmail").unwrap();
        assert!(reloaded.marketplace().is_empty());
    }

    #[test]
    fn marketplace_keeps_corrupt_choices_until_recovered() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(MARKETPLACE_FILE), "not json").unwrap();
        let plugins = Plugins::load(dir.path());
        assert!(plugins.marketplace().is_empty());
        assert!(plugins.add_marketplace("composio.gmail").is_err());
        assert_eq!(
            std::fs::read_to_string(dir.path().join(MARKETPLACE_FILE)).unwrap(),
            "not json"
        );
    }
}
