//! Account-bound model execution. Storage and credentials come from ark-connect.

use crate::{
    Completion, GeneratedImage, Message, Provider, ProviderError, Result, TextSink, ToolDef, WebHit,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountState {
    #[default]
    Active,
    Reconnecting,
    Removing,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelAccount {
    pub id: String,
    pub provider: String,
    pub name: String,
    #[serde(default)]
    pub identity: Option<String>,
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub state: AccountState,
    #[serde(default)]
    pub removed: bool,
}

impl ModelAccount {
    pub fn check(&self) -> Result<()> {
        let current = get(&self.id)?.ok_or_else(|| unavailable(&self.id))?;
        if current.revision != self.revision
            || current.provider != self.provider
            || current.state != AccountState::Active
        {
            return Err(unavailable(&self.id));
        }
        Ok(())
    }
    pub fn directory(&self) -> Result<PathBuf> {
        self.check()?;
        crate::keys::source()
            .and_then(|s| s.account_directory(&self.id))
            .ok_or_else(|| unavailable(&self.id))
    }
    pub fn key(&self) -> Result<String> {
        self.check()?;
        crate::keys::source()
            .and_then(|s| s.account_key(&self.id))
            .ok_or_else(|| unavailable(&self.id))
    }
    pub fn setup_command(&self, cmd: &mut tokio::process::Command) -> Result<()> {
        let current = get(&self.id)?.ok_or_else(|| unavailable(&self.id))?;
        if current.revision != self.revision
            || current.provider != self.provider
            || current.state == AccountState::Removing
        {
            return Err(unavailable(&self.id));
        }
        let dir = crate::keys::source()
            .and_then(|s| s.account_directory(&self.id))
            .ok_or_else(|| unavailable(&self.id))?;
        self.configure(cmd, dir)
    }

    pub fn login_command(&self, cmd: &mut tokio::process::Command) -> Result<()> {
        self.setup_command(cmd)?;
        let dir = crate::keys::source()
            .and_then(|s| s.account_directory(&self.id))
            .ok_or_else(|| unavailable(&self.id))?;
        match std::fs::remove_file(dir.join(".ark-setup-unused")) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(unavailable(&self.id)),
        }
        Ok(())
    }

    pub fn command(&self, cmd: &mut tokio::process::Command) -> Result<()> {
        self.configure(cmd, self.directory()?)
    }

    fn configure(&self, cmd: &mut tokio::process::Command, dir: PathBuf) -> Result<()> {
        match self.provider.as_str() {
            "claude" => {
                for name in [
                    "ANTHROPIC_API_KEY",
                    "ANTHROPIC_AUTH_TOKEN",
                    "CLAUDE_CODE_OAUTH_TOKEN",
                    "CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR",
                    "ANTHROPIC_BASE_URL",
                    "CLAUDE_CODE_USE_BEDROCK",
                    "CLAUDE_CODE_USE_VERTEX",
                    "CLAUDE_CODE_USE_FOUNDRY",
                ] {
                    cmd.env_remove(name);
                }
                cmd.env("CLAUDE_CONFIG_DIR", dir);
            }
            "codex" => {
                for name in [
                    "OPENAI_API_KEY",
                    "CODEX_API_KEY",
                    "OPENAI_IDENTITY_TOKEN",
                    "OPENAI_IDENTITY_TOKEN_FILE",
                    "CODEX_SQLITE_HOME",
                    "OPENAI_BASE_URL",
                ] {
                    cmd.env_remove(name);
                }
                cmd.env("CODEX_HOME", dir);
            }
            _ => return Err(unavailable(&self.id)),
        }
        Ok(())
    }
    pub async fn logout(&self) -> Result<()> {
        let current = get(&self.id)?.ok_or_else(|| unavailable(&self.id))?;
        if current.state != AccountState::Removing
            || current.revision != self.revision
            || active(&self.id) != 0
        {
            return Err(unavailable(&self.id));
        }
        let Some(cli) = crate::Cli::of(&self.provider) else {
            return Ok(());
        };
        let Some(bin) = cli.locate() else {
            let unused = crate::keys::source()
                .and_then(|s| s.account_directory(&self.id))
                .is_some_and(|dir| dir.join(".ark-setup-unused").is_file());
            if self.identity.is_none() && unused {
                return Ok(());
            }
            return Err(ProviderError::MissingCredential(format!(
                "Install {} to remove its saved sign-in.",
                cli.name()
            )));
        };
        let dir = crate::keys::source()
            .and_then(|s| s.account_directory(&self.id))
            .ok_or_else(|| unavailable(&self.id))?;
        let mut cmd = crate::cli::command(&bin);
        self.configure(&mut cmd, dir)?;
        if cli == crate::Cli::Claude {
            cmd.args(["auth", "logout"]);
        } else {
            cmd.arg("logout");
        }
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|_| unavailable(&self.id))?;
        let mut tree = ark_os::process::Tree::of(child.id());
        let status = tokio::time::timeout(std::time::Duration::from_secs(15), child.wait())
            .await
            .map_err(|_| unavailable(&self.id))?
            .map_err(|_| unavailable(&self.id))?;
        if !status.success() {
            return Err(unavailable(&self.id));
        }
        tree.disarm();
        Ok(())
    }

    pub(crate) fn record(&self, mut report: crate::SubscriptionUsage) {
        if self.check().is_ok() {
            report.account = self.id.clone();
            crate::usage::record_account(self, report);
        }
    }
}

pub fn list() -> Result<Vec<ModelAccount>> {
    match crate::keys::source() {
        Some(source) => source.model_accounts(),
        None => Ok(vec![]),
    }
}
pub fn get(id: &str) -> Result<Option<ModelAccount>> {
    Ok(list()?.into_iter().find(|a| a.id == id))
}
pub fn provider(id: &str) -> &str {
    id.split_once('~').map_or(id, |(p, _)| p)
}
fn unavailable(id: &str) -> ProviderError {
    ProviderError::Api {
        provider: provider(id).into(),
        detail: "This account is unavailable. Choose an account or reconnect it in Models.".into(),
    }
}

pub(crate) fn build(account: ModelAccount, model: String) -> Result<Box<dyn Provider>> {
    account.check()?;
    if let Some(cli) = crate::Cli::of(&account.provider) {
        return Ok(Box::new(crate::CliModel::for_account(cli, model, account)?));
    }
    let key = account.key()?;
    let inner: Arc<dyn Provider> = match account.provider.as_str() {
        "anthropic" => Arc::new(
            crate::Anthropic::new(&model, key).with_base_url(
                std::env::var("ANTHROPIC_BASE_URL")
                    .unwrap_or_else(|_| "https://api.anthropic.com/v1".into()),
            ),
        ),
        "openai" => Arc::new(
            crate::OpenAi::new(
                std::env::var("OPENAI_BASE_URL")
                    .unwrap_or_else(|_| "https://api.openai.com/v1".into()),
                &model,
                key,
            )
            .reads_pictures(true),
        ),
        "opencode-go" => Arc::new(crate::OpenCodeGo::new(&model)?.with_account(account.clone())),
        other => {
            let endpoint =
                crate::endpoints::endpoint(other).ok_or_else(|| unavailable(&account.id))?;
            Arc::new(
                crate::OpenAi::new(endpoint.base_url, &model, key)
                    .named(other)
                    .with_context_window(endpoint.context_window),
            )
        }
    };
    Ok(Box::new(AccountProvider {
        account,
        model,
        inner,
    }))
}
struct AccountProvider {
    account: ModelAccount,
    model: String,
    inner: Arc<dyn Provider>,
}
#[async_trait]
impl Provider for AccountProvider {
    fn host_controls_tools(&self) -> bool {
        self.inner.host_controls_tools()
    }
    fn connection_revision(&self) -> u64 {
        self.account.revision
    }
    fn describe(&self) -> String {
        format!("{} {}", self.account.id, self.model)
    }
    fn model_id(&self) -> String {
        format!("{}:{}", self.account.id, self.model)
    }
    fn reads_pictures(&self) -> bool {
        self.inner.reads_pictures()
    }
    fn context_window(&self) -> Option<u32> {
        self.inner.context_window()
    }
    fn generates_images(&self) -> bool {
        self.inner.generates_images()
    }
    fn for_conversation(&self, conversation: &str) -> Result<Option<Arc<dyn Provider>>> {
        let current = get(&self.account.id)?.ok_or_else(|| unavailable(&self.account.id))?;
        current.check()?;
        if current.revision != self.account.revision {
            let fresh = build(current, self.model.clone())?;
            let bound = fresh.for_conversation(conversation)?;
            return Ok(Some(bound.unwrap_or_else(|| Arc::from(fresh))));
        }
        Ok(self.inner.for_conversation(conversation)?.map(|inner| {
            Arc::new(Self {
                account: self.account.clone(),
                model: self.model.clone(),
                inner,
            }) as Arc<dyn Provider>
        }))
    }
    async fn complete(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> Result<Completion> {
        let _lease = Lease::acquire(&self.account)?;
        self.inner.complete(system, messages, tools).await
    }
    async fn complete_streaming(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDef],
        on_text: TextSink<'_>,
    ) -> Result<Completion> {
        let _lease = Lease::acquire(&self.account)?;
        self.inner
            .complete_streaming(system, messages, tools, on_text)
            .await
    }
    async fn web_search(&self, query: &str, limit: usize) -> Option<Result<Vec<WebHit>>> {
        let _lease = match Lease::acquire(&self.account) {
            Ok(lease) => lease,
            Err(e) => return Some(Err(e)),
        };
        self.inner.web_search(query, limit).await
    }
    async fn generate_image(&self, prompt: &str) -> Option<Result<GeneratedImage>> {
        let _lease = match Lease::acquire(&self.account) {
            Ok(lease) => lease,
            Err(e) => return Some(Err(e)),
        };
        self.inner.generate_image(prompt).await
    }
}

static ACTIVE: LazyLock<Mutex<HashMap<String, usize>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
pub struct Lease {
    id: String,
}
impl Lease {
    pub fn login(account: &ModelAccount) -> Result<Self> {
        {
            let mut active = ACTIVE.lock().unwrap();
            if active.get(&account.id).copied().unwrap_or(0) != 0 {
                return Err(ProviderError::Api {
                    provider: account.provider.clone(),
                    detail: "This account is busy. Try again when its request finishes.".into(),
                });
            }
            active.insert(account.id.clone(), 1);
        }
        let lease = Self {
            id: account.id.clone(),
        };
        let current = get(&account.id)?.ok_or_else(|| unavailable(&account.id))?;
        if current.state != AccountState::Reconnecting
            || current.revision != account.revision
            || current.provider != account.provider
        {
            return Err(unavailable(&account.id));
        }
        Ok(lease)
    }

    pub fn setup(account: &ModelAccount) -> Result<Self> {
        let lease = Self {
            id: account.id.clone(),
        };
        *ACTIVE.lock().unwrap().entry(lease.id.clone()).or_default() += 1;
        let current = get(&account.id)?.ok_or_else(|| unavailable(&account.id))?;
        if current.revision != account.revision
            || current.provider != account.provider
            || current.state == AccountState::Removing
        {
            return Err(unavailable(&account.id));
        }
        Ok(lease)
    }
    pub fn acquire(account: &ModelAccount) -> Result<Self> {
        let lease = Self {
            id: account.id.clone(),
        };
        *ACTIVE.lock().unwrap().entry(lease.id.clone()).or_default() += 1;
        account.check()?;
        Ok(lease)
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        let mut active = ACTIVE.lock().unwrap();
        if let Some(count) = active.get_mut(&self.id) {
            *count -= 1;
            if *count == 0 {
                active.remove(&self.id);
            }
        }
    }
}
pub fn active(id: &str) -> usize {
    ACTIVE.lock().unwrap().get(id).copied().unwrap_or(0)
}

#[async_trait]
pub(crate) trait AccountProcesses: Send + Sync {
    async fn stop(&self);
}
type WeakProcesses = std::sync::Weak<dyn AccountProcesses>;
static PROCESSES: LazyLock<Mutex<HashMap<String, Vec<WeakProcesses>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
pub(crate) fn register_processes(id: &str, processes: Arc<dyn AccountProcesses>) {
    let mut all = PROCESSES.lock().unwrap();
    let entries = all.entry(id.into()).or_default();
    entries.retain(|p| p.strong_count() > 0);
    entries.push(Arc::downgrade(&processes));
}
pub async fn stop_processes(id: &str) -> Result<()> {
    if active(id) != 0 {
        return Err(unavailable(id));
    }
    let all = PROCESSES
        .lock()
        .unwrap()
        .get(id)
        .cloned()
        .unwrap_or_default();
    for weak in all {
        if let Some(processes) = weak.upgrade() {
            processes.stop().await;
        }
    }
    Ok(())
}

pub fn unavailable_selection(provider: &str, model: &str, reason: String) -> Arc<dyn Provider> {
    Arc::new(UnavailableSelection {
        provider: provider.into(),
        model: model.into(),
        reason,
    })
}
struct UnavailableSelection {
    provider: String,
    model: String,
    reason: String,
}
#[async_trait]
impl Provider for UnavailableSelection {
    fn for_conversation(&self, conversation: &str) -> Result<Option<Arc<dyn Provider>>> {
        let fresh = crate::build(&self.provider, Some(self.model.clone()))?;
        Ok(Some(fresh.for_conversation(conversation)?.unwrap_or(fresh)))
    }

    fn describe(&self) -> String {
        format!("{} {}", self.provider, self.model)
    }
    fn model_id(&self) -> String {
        format!("{}:{}", self.provider, self.model)
    }
    async fn complete(
        &self,
        _system: &str,
        _messages: &[Message],
        _tools: &[ToolDef],
    ) -> Result<Completion> {
        Err(ProviderError::Api {
            provider: provider(&self.provider).into(),
            detail: self.reason.clone(),
        })
    }
}
