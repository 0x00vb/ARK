use crate::Cli;
use ark_os::process::{command, Tree};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::sync::{oneshot, Mutex as AsyncMutex, Notify};
use url::Url;

#[cfg(not(test))]
const LOGIN_URL_DEADLINE: Duration = Duration::from_secs(15);
#[cfg(test)]
const LOGIN_URL_DEADLINE: Duration = Duration::from_millis(250);
const ATTEMPT_TTL: Duration = Duration::from_secs(600);
const TERMINAL_GRACE: Duration = Duration::from_secs(60);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const CHILD_REAP_TIMEOUT: Duration = Duration::from_secs(1);
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);
const STATUS_CACHE_TTL: Duration = Duration::from_secs(5);
const OUTPUT_BYTES: usize = 64 * 1024;
const URL_BYTES: usize = 16 * 1024;
const ATTEMPTS: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionState {
    Ready,
    NeedsAuthorization,
    Authorizing,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttemptState {
    WaitingForCode,
    WaitingForBrowser,
    Verifying,
    Ready,
    Failed,
    Cancelled,
    Expired,
}

#[derive(Clone, PartialEq, Eq)]
pub struct AttemptView {
    pub id: String,
    pub connection_id: String,
    pub state: AttemptState,
    pub authorization_url: Option<String>,
    pub user_code: Option<String>,
    pub expires_unix_seconds: u64,
    pub display: String,
}

impl std::fmt::Debug for AttemptView {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AttemptView")
            .field("id", &self.id)
            .field("connection_id", &self.connection_id)
            .field("state", &self.state)
            .field(
                "authorization_url",
                &self.authorization_url.as_ref().map(|_| "[redacted]"),
            )
            .field("user_code", &self.user_code.as_ref().map(|_| "[redacted]"))
            .field("expires_unix_seconds", &self.expires_unix_seconds)
            .field("display", &self.display)
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthorizationError {
    InvalidRequest,
    InvalidCode,
    Missing,
    InProgress,
    CodeUsed,
    AlreadyReady,
    Unavailable,
    CleanupFailed,
    Capacity { retry_after_seconds: u64 },
    Failed,
}

#[derive(Clone)]
pub struct ClaudeAuthorization {
    inner: Arc<Inner>,
}

static CLAUDE_AUTHORIZATION: LazyLock<ClaudeAuthorization> =
    LazyLock::new(ClaudeAuthorization::new);

pub fn global_claude_authorization() -> ClaudeAuthorization {
    CLAUDE_AUTHORIZATION.clone()
}

struct Inner {
    state: Mutex<State>,
    status_gate: AsyncMutex<()>,
    operation_gate: AsyncMutex<()>,
}

struct State {
    active: Option<String>,
    attempts: HashMap<String, Attempt>,
    status: HashMap<Cli, CachedStatus>,
    blocked: bool,
}

struct CachedStatus {
    checked: Instant,
    state: ConnectionState,
}

struct Attempt {
    id: String,
    provider: Cli,
    route_id: String,
    owner: String,
    listener_id: String,
    request_id: String,
    expires: Instant,
    expires_unix_seconds: u64,
    terminal_at: Option<Instant>,
    state: InternalAttemptState,
    code_used: bool,
    code: Option<oneshot::Sender<Vec<u8>>>,
    cancel: Option<oneshot::Sender<()>>,
    notify: Arc<Notify>,
    cleanup: Cleanup,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Cleanup {
    Pending,
    Stopped,
    Failed,
}

enum InternalAttemptState {
    Starting,
    WaitingForCode(String),
    WaitingForBrowser { url: String, code: String },
    Verifying,
    Ready,
    Failed,
    Cancelled,
    Expired,
}

impl Default for ClaudeAuthorization {
    fn default() -> Self {
        Self::new()
    }
}

impl ClaudeAuthorization {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(State {
                    active: None,
                    attempts: HashMap::new(),
                    status: HashMap::new(),
                    blocked: false,
                }),
                status_gate: AsyncMutex::new(()),
                operation_gate: AsyncMutex::new(()),
            }),
        }
    }

    pub async fn connection(&self) -> ConnectionState {
        self.connection_for(Cli::Claude).await
    }

    pub async fn connection_for(&self, provider: Cli) -> ConnectionState {
        {
            let state = self.inner.state.lock().expect("authorization state");
            if state.blocked
                || state
                    .active
                    .as_ref()
                    .and_then(|id| state.attempts.get(id))
                    .is_some_and(|attempt| attempt.cleanup == Cleanup::Failed)
            {
                return ConnectionState::Unavailable;
            }
            if state
                .active
                .as_ref()
                .and_then(|id| state.attempts.get(id))
                .is_some_and(|attempt| attempt.provider == provider)
            {
                return ConnectionState::Authorizing;
            }
            if let Some(cached) = state.status.get(&provider) {
                if cached.checked.elapsed() < STATUS_CACHE_TTL {
                    return cached.state;
                }
            }
        }
        self.probe_status(provider, false).await
    }

    pub async fn begin(
        &self,
        route_id: &str,
        owner: &str,
        listener_id: &str,
        request_id: &str,
    ) -> Result<AttemptView, AuthorizationError> {
        self.begin_for(Cli::Claude, route_id, owner, listener_id, request_id)
            .await
    }

    pub async fn begin_for(
        &self,
        provider: Cli,
        route_id: &str,
        owner: &str,
        listener_id: &str,
        request_id: &str,
    ) -> Result<AttemptView, AuthorizationError> {
        if route_id.is_empty()
            || owner.is_empty()
            || listener_id.is_empty()
            || request_id.is_empty()
            || request_id.len() > 128
            || !request_id
                .bytes()
                .all(|byte| byte.is_ascii_graphic() || byte == b' ')
        {
            return Err(AuthorizationError::InvalidRequest);
        }
        let operation = self.inner.operation_gate.lock().await;
        let existing = {
            let mut state = self.inner.state.lock().expect("authorization state");
            prune(&mut state);
            if let Some(active_id) = &state.active {
                let attempt = state.attempts.get(active_id).expect("active attempt");
                if attempt.route_id == route_id
                    && attempt.owner == owner
                    && attempt.listener_id == listener_id
                    && attempt.provider == provider
                    && attempt.request_id == request_id
                {
                    Some(attempt.id.clone())
                } else {
                    return Err(AuthorizationError::InProgress);
                }
            } else {
                state
                    .attempts
                    .values()
                    .find(|attempt| {
                        attempt.route_id == route_id
                            && attempt.owner == owner
                            && attempt.listener_id == listener_id
                            && attempt.provider == provider
                            && attempt.request_id == request_id
                    })
                    .map(|attempt| attempt.id.clone())
            }
        };
        if let Some(id) = existing {
            drop(operation);
            return self
                .wait_until_visible(route_id, owner, listener_id, &id)
                .await;
        }
        match self.connection_for(provider).await {
            ConnectionState::Ready => return Err(AuthorizationError::AlreadyReady),
            ConnectionState::Unavailable => return Err(AuthorizationError::Unavailable),
            ConnectionState::Authorizing => return Err(AuthorizationError::InProgress),
            ConnectionState::NeedsAuthorization => {}
        }
        let (code_tx, code_rx) = oneshot::channel();
        let (cancel_tx, cancel_rx) = oneshot::channel();
        let id = uuid::Uuid::new_v4().to_string();
        let notify = Arc::new(Notify::new());
        let now = Instant::now();
        let expires_unix_seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .saturating_add(ATTEMPT_TTL)
            .as_secs();
        {
            let mut state = self.inner.state.lock().expect("authorization state");
            prune(&mut state);
            if state.active.is_some() {
                return Err(AuthorizationError::InProgress);
            }
            if state.attempts.len() >= ATTEMPTS {
                let retry_after_seconds = state
                    .attempts
                    .values()
                    .map(retain_until)
                    .min()
                    .map(|deadline| deadline.saturating_duration_since(now).as_secs().max(1))
                    .unwrap_or(1);
                return Err(AuthorizationError::Capacity {
                    retry_after_seconds,
                });
            }
            state.active = Some(id.clone());
            state.attempts.insert(
                id.clone(),
                Attempt {
                    id: id.clone(),
                    provider,
                    route_id: route_id.to_string(),
                    owner: owner.to_string(),
                    listener_id: listener_id.to_string(),
                    request_id: request_id.to_string(),
                    expires: now + ATTEMPT_TTL,
                    expires_unix_seconds,
                    terminal_at: None,
                    state: InternalAttemptState::Starting,
                    code_used: false,
                    code: Some(code_tx),
                    cancel: Some(cancel_tx),
                    notify,
                    cleanup: Cleanup::Pending,
                },
            );
        }
        let inner = Arc::clone(&self.inner);
        let run_id = id.clone();
        tokio::spawn(async move {
            match provider {
                Cli::Claude => run_claude_login(inner, run_id, code_rx, cancel_rx).await,
                Cli::Codex => run_codex_login(inner, run_id, cancel_rx).await,
            }
        });
        drop(operation);
        self.wait_until_visible(route_id, owner, listener_id, &id)
            .await
    }

    pub fn attempt(
        &self,
        route_id: &str,
        owner: &str,
        listener_id: &str,
        id: &str,
    ) -> Result<AttemptView, AuthorizationError> {
        let mut state = self.inner.state.lock().expect("authorization state");
        prune(&mut state);
        let attempt = state
            .attempts
            .get(id)
            .filter(|attempt| {
                attempt.route_id == route_id
                    && attempt.owner == owner
                    && attempt.listener_id == listener_id
            })
            .ok_or(AuthorizationError::Missing)?;
        visible(attempt).ok_or(AuthorizationError::Missing)
    }

    pub fn submit(
        &self,
        route_id: &str,
        owner: &str,
        listener_id: &str,
        id: &str,
        code: &str,
    ) -> Result<AttemptView, AuthorizationError> {
        if !authorization_code_valid(code) {
            return Err(AuthorizationError::InvalidCode);
        }
        let mut state = self.inner.state.lock().expect("authorization state");
        prune(&mut state);
        let attempt = state
            .attempts
            .get_mut(id)
            .filter(|attempt| {
                attempt.route_id == route_id
                    && attempt.owner == owner
                    && attempt.listener_id == listener_id
            })
            .ok_or(AuthorizationError::Missing)?;
        if attempt.code_used {
            return Err(AuthorizationError::CodeUsed);
        }
        if !matches!(attempt.state, InternalAttemptState::WaitingForCode(_)) {
            return Err(AuthorizationError::InvalidRequest);
        }
        let Some(sender) = attempt.code.take() else {
            return Err(AuthorizationError::CodeUsed);
        };
        attempt.code_used = true;
        attempt.state = InternalAttemptState::Verifying;
        let view = visible(attempt).expect("verifying attempt is visible");
        if sender.send(code.as_bytes().to_vec()).is_err() {
            mark_terminal_locked(&mut state, id, InternalAttemptState::Failed);
            fail_cleanup_locked(&mut state, id);
            return Err(AuthorizationError::Failed);
        }
        Ok(view)
    }

    pub async fn cancel(
        &self,
        route_id: &str,
        owner: &str,
        listener_id: &str,
        id: &str,
    ) -> Result<AttemptView, AuthorizationError> {
        {
            let mut state = self.inner.state.lock().expect("authorization state");
            prune(&mut state);
            let attempt = state
                .attempts
                .get_mut(id)
                .filter(|attempt| {
                    attempt.route_id == route_id
                        && attempt.owner == owner
                        && attempt.listener_id == listener_id
                })
                .ok_or(AuthorizationError::Missing)?;
            if !is_terminal(&attempt.state) {
                let delivered = attempt
                    .cancel
                    .take()
                    .is_some_and(|cancel| cancel.send(()).is_ok());
                mark_terminal_locked(&mut state, id, InternalAttemptState::Cancelled);
                if !delivered {
                    fail_cleanup_locked(&mut state, id);
                }
            }
        }
        self.wait_until_drained(route_id, owner, listener_id, id)
            .await?;
        self.attempt(route_id, owner, listener_id, id)
    }

    pub fn cancel_if_owner_revoked(
        &self,
        listener_id: &str,
        owner_is_live: impl FnOnce(&str) -> bool,
    ) {
        let mut state = self.inner.state.lock().expect("authorization state");
        let Some(id) = state.active.clone() else {
            return;
        };
        let revoked = state.attempts.get(&id).is_some_and(|attempt| {
            attempt.listener_id == listener_id && !owner_is_live(&attempt.owner)
        });
        if revoked {
            let delivered = state
                .attempts
                .get_mut(&id)
                .and_then(|attempt| attempt.cancel.take())
                .is_some_and(|cancel| cancel.send(()).is_ok());
            mark_terminal_locked(&mut state, &id, InternalAttemptState::Cancelled);
            if !delivered {
                fail_cleanup_locked(&mut state, &id);
            }
        }
    }

    pub fn cancel_listener(&self, listener_id: &str) {
        let mut state = self.inner.state.lock().expect("authorization state");
        let Some(id) = state.active.clone() else {
            return;
        };
        let owned = state
            .attempts
            .get(&id)
            .is_some_and(|attempt| attempt.listener_id == listener_id);
        if !owned {
            return;
        }
        let delivered = state
            .attempts
            .get_mut(&id)
            .and_then(|attempt| attempt.cancel.take())
            .is_some_and(|cancel| cancel.send(()).is_ok());
        mark_terminal_locked(&mut state, &id, InternalAttemptState::Cancelled);
        if !delivered {
            fail_cleanup_locked(&mut state, &id);
        }
    }

    pub async fn disconnect(&self) -> Result<ConnectionState, AuthorizationError> {
        self.disconnect_for(Cli::Claude).await
    }

    pub async fn disconnect_for(
        &self,
        provider: Cli,
    ) -> Result<ConnectionState, AuthorizationError> {
        let _operation = self.inner.operation_gate.lock().await;
        if self
            .inner
            .state
            .lock()
            .expect("authorization state")
            .blocked
        {
            return Err(AuthorizationError::Unavailable);
        }
        if self
            .inner
            .state
            .lock()
            .expect("authorization state")
            .active
            .is_some()
        {
            return Err(AuthorizationError::InProgress);
        }
        let _gate = self.inner.status_gate.lock().await;
        let before = strict_status(&self.inner, provider).await;
        if before == ConnectionState::NeedsAuthorization {
            self.inner
                .state
                .lock()
                .expect("authorization state")
                .status
                .insert(
                    provider,
                    CachedStatus {
                        checked: Instant::now(),
                        state: before,
                    },
                );
            return Ok(ConnectionState::NeedsAuthorization);
        }
        if before != ConnectionState::Ready {
            return Err(AuthorizationError::Unavailable);
        }
        let Some(program) = provider.locate() else {
            return Err(AuthorizationError::Unavailable);
        };
        let args: &[&str] = match provider {
            Cli::Claude => &["auth", "logout"],
            Cli::Codex => &["logout"],
        };
        let output = match run_bounded(&program, args).await {
            Err(AuthorizationError::CleanupFailed) => {
                self.inner
                    .state
                    .lock()
                    .expect("authorization state")
                    .blocked = true;
                return Err(AuthorizationError::Unavailable);
            }
            result => result?,
        };
        if !output.success {
            return Err(AuthorizationError::Failed);
        }
        let state = strict_status(&self.inner, provider).await;
        self.inner
            .state
            .lock()
            .expect("authorization state")
            .status
            .insert(
                provider,
                CachedStatus {
                    checked: Instant::now(),
                    state,
                },
            );
        if state == ConnectionState::NeedsAuthorization {
            Ok(state)
        } else {
            Err(AuthorizationError::Failed)
        }
    }

    async fn wait_until_visible(
        &self,
        route_id: &str,
        owner: &str,
        listener_id: &str,
        id: &str,
    ) -> Result<AttemptView, AuthorizationError> {
        let notify = {
            let state = self.inner.state.lock().expect("authorization state");
            Arc::clone(
                &state
                    .attempts
                    .get(id)
                    .filter(|attempt| {
                        attempt.route_id == route_id
                            && attempt.owner == owner
                            && attempt.listener_id == listener_id
                    })
                    .ok_or(AuthorizationError::Missing)?
                    .notify,
            )
        };
        loop {
            let notified = notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let state = self.inner.state.lock().expect("authorization state");
                let attempt = state
                    .attempts
                    .get(id)
                    .filter(|attempt| {
                        attempt.route_id == route_id
                            && attempt.owner == owner
                            && attempt.listener_id == listener_id
                    })
                    .ok_or(AuthorizationError::Missing)?;
                if let Some(view) = visible(attempt) {
                    return Ok(view);
                }
            }
            if tokio::time::timeout(LOGIN_URL_DEADLINE + Duration::from_secs(1), notified)
                .await
                .is_err()
            {
                return Err(AuthorizationError::Failed);
            }
        }
    }

    async fn wait_until_drained(
        &self,
        route_id: &str,
        owner: &str,
        listener_id: &str,
        id: &str,
    ) -> Result<(), AuthorizationError> {
        let notify = {
            let state = self.inner.state.lock().expect("authorization state");
            Arc::clone(
                &state
                    .attempts
                    .get(id)
                    .filter(|attempt| {
                        attempt.route_id == route_id
                            && attempt.owner == owner
                            && attempt.listener_id == listener_id
                    })
                    .ok_or(AuthorizationError::Missing)?
                    .notify,
            )
        };
        let deadline = tokio::time::Instant::now() + CLEANUP_TIMEOUT;
        loop {
            let notified = notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let state = self.inner.state.lock().expect("authorization state");
                let attempt = state
                    .attempts
                    .get(id)
                    .filter(|attempt| {
                        attempt.route_id == route_id
                            && attempt.owner == owner
                            && attempt.listener_id == listener_id
                    })
                    .ok_or(AuthorizationError::Missing)?;
                match attempt.cleanup {
                    Cleanup::Stopped => return Ok(()),
                    Cleanup::Failed => return Err(AuthorizationError::Failed),
                    Cleanup::Pending => {}
                }
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return Err(AuthorizationError::Failed);
            }
        }
    }

    async fn probe_status(&self, provider: Cli, bypass_cache: bool) -> ConnectionState {
        let _gate = self.inner.status_gate.lock().await;
        if !bypass_cache {
            let state = self.inner.state.lock().expect("authorization state");
            if let Some(cached) = state.status.get(&provider) {
                if cached.checked.elapsed() < STATUS_CACHE_TTL {
                    return cached.state;
                }
            }
        }
        let checked = strict_status(&self.inner, provider).await;
        self.inner
            .state
            .lock()
            .expect("authorization state")
            .status
            .insert(
                provider,
                CachedStatus {
                    checked: Instant::now(),
                    state: checked,
                },
            );
        checked
    }
}

fn visible(attempt: &Attempt) -> Option<AttemptView> {
    let provider = attempt.provider.label();
    let (state, authorization_url, user_code, display) = match &attempt.state {
        InternalAttemptState::Starting => return None,
        InternalAttemptState::WaitingForCode(url) => (
            AttemptState::WaitingForCode,
            Some(url.clone()),
            None,
            format!("Finish signing in to {provider}."),
        ),
        InternalAttemptState::WaitingForBrowser { url, code } => (
            AttemptState::WaitingForBrowser,
            Some(url.clone()),
            Some(code.clone()),
            format!("Finish signing in to {provider}."),
        ),
        InternalAttemptState::Verifying => (
            AttemptState::Verifying,
            None,
            None,
            format!("Checking your {provider} account…"),
        ),
        InternalAttemptState::Ready => (
            AttemptState::Ready,
            None,
            None,
            format!("{provider} is connected."),
        ),
        InternalAttemptState::Failed => (
            AttemptState::Failed,
            None,
            None,
            format!("{provider} could not be connected. Try again."),
        ),
        InternalAttemptState::Cancelled => (
            AttemptState::Cancelled,
            None,
            None,
            "Sign-in was cancelled.".into(),
        ),
        InternalAttemptState::Expired => (
            AttemptState::Expired,
            None,
            None,
            "This sign-in expired. Start again.".into(),
        ),
    };
    Some(AttemptView {
        id: attempt.id.clone(),
        connection_id: attempt.provider.name().to_string(),
        state,
        authorization_url,
        user_code,
        expires_unix_seconds: attempt.expires_unix_seconds,
        display,
    })
}

fn prune(state: &mut State) {
    let now = Instant::now();
    let active = state.active.clone();
    state.attempts.retain(|id, attempt| {
        active.as_deref() == Some(id.as_str())
            || attempt.terminal_at.is_none()
            || retain_until(attempt) > now
    });
}

fn retain_until(attempt: &Attempt) -> Instant {
    attempt
        .terminal_at
        .map(|terminal| attempt.expires.max(terminal + TERMINAL_GRACE))
        .unwrap_or(attempt.expires)
}

fn is_terminal(state: &InternalAttemptState) -> bool {
    matches!(
        state,
        InternalAttemptState::Ready
            | InternalAttemptState::Failed
            | InternalAttemptState::Cancelled
            | InternalAttemptState::Expired
    )
}

fn finish_locked(state: &mut State, id: &str, terminal: InternalAttemptState) {
    mark_terminal_locked(state, id, terminal);
    release_locked(state, id);
}

fn release_locked(state: &mut State, id: &str) {
    mark_drained_locked(state, id);
    if state.active.as_deref() == Some(id) {
        state.active = None;
    }
}

fn mark_drained_locked(state: &mut State, id: &str) {
    if let Some(attempt) = state.attempts.get_mut(id) {
        attempt.cleanup = Cleanup::Stopped;
        attempt.notify.notify_waiters();
    }
}

fn fail_cleanup_locked(state: &mut State, id: &str) {
    if let Some(attempt) = state.attempts.get_mut(id) {
        attempt.cleanup = Cleanup::Failed;
        attempt.notify.notify_waiters();
    }
}

fn mark_terminal_locked(state: &mut State, id: &str, terminal: InternalAttemptState) {
    if let Some(attempt) = state.attempts.get_mut(id) {
        if !is_terminal(&attempt.state) {
            attempt.state = terminal;
            attempt.terminal_at = Some(Instant::now());
            attempt.code = None;
            attempt.cancel = None;
            attempt.notify.notify_waiters();
        }
    }
}

async fn run_claude_login(
    inner: Arc<Inner>,
    id: String,
    mut code: oneshot::Receiver<Vec<u8>>,
    mut cancel: oneshot::Receiver<()>,
) {
    let Some(program) = Cli::Claude.locate() else {
        finish(&inner, &id, InternalAttemptState::Failed);
        return;
    };
    let mut cmd = private_command(&program);
    cmd.args(["auth", "login", "--claudeai"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let Ok(mut child) = cmd.spawn() else {
        finish(&inner, &id, InternalAttemptState::Failed);
        return;
    };
    let mut tree = Tree::of(child.id());
    let mut stdin = child.stdin.take();
    let (chunks_tx, mut chunks) = tokio::sync::mpsc::channel(16);
    if let Some(stdout) = child.stdout.take() {
        tokio::spawn(read_chunks(stdout, chunks_tx.clone()));
    }
    if let Some(stderr) = child.stderr.take() {
        tokio::spawn(read_chunks(stderr, chunks_tx));
    }
    let started = Instant::now();
    let expires = started + ATTEMPT_TTL;
    let mut output = Vec::new();
    let mut output_open = true;
    let mut url_ready = false;
    let mut code_written = false;
    let mut tick = tokio::time::interval(Duration::from_millis(25));
    loop {
        tokio::select! {
            _ = &mut cancel => {
                let cleaned = stop_and_reap(&mut tree, &mut child).await;
                finish_after_cleanup(&inner, &id, InternalAttemptState::Cancelled, cleaned);
                return;
            }
            accepted = &mut code, if url_ready && !code_written => {
                let Ok(accepted) = accepted else {
                    let cleaned = stop_and_reap(&mut tree, &mut child).await;
                    finish_after_cleanup(&inner, &id, InternalAttemptState::Cancelled, cleaned);
                    return;
                };
                let Some(mut input) = stdin.take() else {
                    let cleaned = stop_and_reap(&mut tree, &mut child).await;
                    finish_after_cleanup(&inner, &id, InternalAttemptState::Failed, cleaned);
                    return;
                };
                if input.write_all(&accepted).await.is_err()
                    || input.write_all(b"\n").await.is_err()
                    || input.shutdown().await.is_err()
                {
                    let cleaned = stop_and_reap(&mut tree, &mut child).await;
                    finish_after_cleanup(&inner, &id, InternalAttemptState::Failed, cleaned);
                    return;
                }
                code_written = true;
            }
            chunk = chunks.recv(), if output_open => {
                if let Some(chunk) = chunk {
                    if output.len().saturating_add(chunk.len()) > OUTPUT_BYTES {
                        let cleaned = stop_and_reap(&mut tree, &mut child).await;
                        finish_after_cleanup(&inner, &id, InternalAttemptState::Failed, cleaned);
                        return;
                    }
                    output.extend_from_slice(&chunk);
                    if !url_ready {
                        if let Some(url) = authorization_url(&output) {
                            let mut state = inner.state.lock().expect("authorization state");
                            if let Some(attempt) = state.attempts.get_mut(&id) {
                                if matches!(attempt.state, InternalAttemptState::Starting) {
                                    attempt.state = InternalAttemptState::WaitingForCode(url);
                                    attempt.notify.notify_waiters();
                                    url_ready = true;
                                }
                            }
                        }
                    }
                } else { output_open = false; }
            }
            _ = tick.tick() => {
                let now = Instant::now();
                if (!url_ready && now >= started + LOGIN_URL_DEADLINE) || now >= expires {
                    let terminal = if now >= expires { InternalAttemptState::Expired } else { InternalAttemptState::Failed };
                    let cleaned = stop_and_reap(&mut tree, &mut child).await;
                    finish_after_cleanup(&inner, &id, terminal, cleaned);
                    return;
                }
                match child.try_wait() {
                    Ok(Some(status)) => {
                        let cleaned = stop_and_reap(&mut tree, &mut child).await;
                        if !cleaned || !code_written || !status.success() {
                            finish_after_cleanup(
                                &inner,
                                &id,
                                InternalAttemptState::Failed,
                                cleaned,
                            );
                            return;
                        }
                        let ready = strict_status(&inner, Cli::Claude).await == ConnectionState::Ready;
                        inner.state.lock().expect("authorization state").status.insert(Cli::Claude, CachedStatus {
                            checked: Instant::now(),
                            state: if ready { ConnectionState::Ready } else { ConnectionState::NeedsAuthorization },
                        });
                        finish(&inner, &id, if ready { InternalAttemptState::Ready } else { InternalAttemptState::Failed });
                        return;
                    }
                    Ok(None) => {}
                    Err(_) => {
                        let cleaned = stop_and_reap(&mut tree, &mut child).await;
                        finish_after_cleanup(&inner, &id, InternalAttemptState::Failed, cleaned);
                        return;
                    }
                }
            }
        }
    }
}

async fn run_codex_login(inner: Arc<Inner>, id: String, mut cancel: oneshot::Receiver<()>) {
    let Some(program) = Cli::Codex.locate() else {
        finish(&inner, &id, InternalAttemptState::Failed);
        return;
    };
    let mut cmd = private_command(&program);
    cmd.args(["login", "--device-auth"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let Ok(mut child) = cmd.spawn() else {
        finish(&inner, &id, InternalAttemptState::Failed);
        return;
    };
    let mut tree = Tree::of(child.id());
    let (chunks_tx, mut chunks) = tokio::sync::mpsc::channel(16);
    if let Some(stdout) = child.stdout.take() {
        tokio::spawn(read_chunks(stdout, chunks_tx.clone()));
    }
    if let Some(stderr) = child.stderr.take() {
        tokio::spawn(read_chunks(stderr, chunks_tx));
    }
    let started = Instant::now();
    let expires = started + ATTEMPT_TTL;
    let mut output = Vec::new();
    let mut output_open = true;
    let mut prompt_ready = false;
    let mut tick = tokio::time::interval(Duration::from_millis(25));
    loop {
        tokio::select! {
            _ = &mut cancel => {
                let cleaned = stop_and_reap(&mut tree, &mut child).await;
                finish_after_cleanup(&inner, &id, InternalAttemptState::Cancelled, cleaned);
                return;
            }
            chunk = chunks.recv(), if output_open => {
                if let Some(chunk) = chunk {
                    if output.len().saturating_add(chunk.len()) > OUTPUT_BYTES {
                        let cleaned = stop_and_reap(&mut tree, &mut child).await;
                        finish_after_cleanup(&inner, &id, InternalAttemptState::Failed, cleaned);
                        return;
                    }
                    output.extend_from_slice(&chunk);
                    if !prompt_ready {
                        if let Some((url, code)) = codex_device_prompt(&output) {
                            let mut state = inner.state.lock().expect("authorization state");
                            if let Some(attempt) = state.attempts.get_mut(&id) {
                                if matches!(attempt.state, InternalAttemptState::Starting) {
                                    attempt.state = InternalAttemptState::WaitingForBrowser { url, code };
                                    attempt.notify.notify_waiters();
                                    prompt_ready = true;
                                }
                            }
                        }
                    }
                } else {
                    output_open = false;
                }
            }
            _ = tick.tick() => {
                let now = Instant::now();
                if (!prompt_ready && now >= started + LOGIN_URL_DEADLINE) || now >= expires {
                    let terminal = if now >= expires {
                        InternalAttemptState::Expired
                    } else {
                        InternalAttemptState::Failed
                    };
                    let cleaned = stop_and_reap(&mut tree, &mut child).await;
                    finish_after_cleanup(&inner, &id, terminal, cleaned);
                    return;
                }
                match child.try_wait() {
                    Ok(Some(status)) => {
                        let cleaned = stop_and_reap(&mut tree, &mut child).await;
                        if !cleaned || !prompt_ready || !status.success() {
                            finish_after_cleanup(&inner, &id, InternalAttemptState::Failed, cleaned);
                            return;
                        }
                        let ready = strict_status(&inner, Cli::Codex).await == ConnectionState::Ready;
                        inner.state.lock().expect("authorization state").status.insert(
                            Cli::Codex,
                            CachedStatus {
                                checked: Instant::now(),
                                state: if ready {
                                    ConnectionState::Ready
                                } else {
                                    ConnectionState::NeedsAuthorization
                                },
                            },
                        );
                        finish(
                            &inner,
                            &id,
                            if ready {
                                InternalAttemptState::Ready
                            } else {
                                InternalAttemptState::Failed
                            },
                        );
                        return;
                    }
                    Ok(None) => {}
                    Err(_) => {
                        let cleaned = stop_and_reap(&mut tree, &mut child).await;
                        finish_after_cleanup(&inner, &id, InternalAttemptState::Failed, cleaned);
                        return;
                    }
                }
            }
        }
    }
}

fn codex_device_prompt(output: &[u8]) -> Option<(String, String)> {
    let plain = strip_ansi(std::str::from_utf8(output).ok()?);
    let lines: Vec<&str> = plain
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let url = lines
        .iter()
        .position(|line| *line == "https://auth.openai.com/codex/device")?;
    let instruction = lines[url + 1..]
        .iter()
        .position(|line| *line == "2. Enter this one-time code (expires in 15 minutes)")?
        + url
        + 1;
    let code = *lines.get(instruction + 1)?;
    codex_user_code_valid(code).then(|| (lines[url].to_string(), code.to_string()))
}

fn strip_ansi(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut plain = String::with_capacity(value.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == 0x1b && bytes.get(index + 1) == Some(&b'[') {
            index += 2;
            while index < bytes.len() && !bytes[index].is_ascii_alphabetic() {
                index += 1;
            }
            index = index.saturating_add(1);
        } else {
            plain.push(bytes[index] as char);
            index += 1;
        }
    }
    plain
}

fn codex_user_code_valid(code: &str) -> bool {
    !code.is_empty()
        && code.len() <= 64
        && code
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'-')
}

fn finish(inner: &Inner, id: &str, terminal: InternalAttemptState) {
    finish_locked(
        &mut inner.state.lock().expect("authorization state"),
        id,
        terminal,
    );
}

fn finish_after_cleanup(inner: &Inner, id: &str, terminal: InternalAttemptState, cleaned: bool) {
    let mut state = inner.state.lock().expect("authorization state");
    mark_terminal_locked(&mut state, id, terminal);
    if cleaned {
        release_locked(&mut state, id);
    } else {
        fail_cleanup_locked(&mut state, id);
    }
}

async fn stop_and_reap(tree: &mut Tree, child: &mut tokio::process::Child) -> bool {
    tree.terminate();
    let (stopped, reaped) = tokio::join!(
        tree.stop(),
        tokio::time::timeout(CHILD_REAP_TIMEOUT, child.wait())
    );
    let stopped = stopped.is_ok();
    let reaped = reaped.is_ok_and(|result| result.is_ok());
    if !stopped {
        tree.kill();
    }
    if !reaped {
        let _ = child.start_kill();
    }
    stopped && reaped
}

async fn read_chunks(
    mut input: impl AsyncRead + Unpin,
    sender: tokio::sync::mpsc::Sender<Vec<u8>>,
) {
    let mut buffer = [0_u8; 4096];
    loop {
        match input.read(&mut buffer).await {
            Ok(0) | Err(_) => return,
            Ok(read) if sender.send(buffer[..read].to_vec()).await.is_err() => return,
            Ok(_) => {}
        }
    }
}

fn private_command(program: &Path) -> tokio::process::Command {
    let mut cmd = command(program);
    cmd.env_clear();
    for name in [
        "HOME",
        "PATH",
        "TMPDIR",
        "LANG",
        "LC_ALL",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
    ] {
        if let Some(value) = std::env::var_os(name) {
            cmd.env(name, value);
        }
    }
    cmd.env("DISABLE_UPDATES", "1");
    cmd
}

struct CommandOutput {
    success: bool,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

async fn run_bounded(program: &Path, args: &[&str]) -> Result<CommandOutput, AuthorizationError> {
    let mut cmd = private_command(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|_| AuthorizationError::Unavailable)?;
    let mut tree = Tree::of(child.id());
    let (sender, mut chunks) = tokio::sync::mpsc::channel(16);
    if let Some(stdout) = child.stdout.take() {
        tokio::spawn(read_tagged(stdout, true, sender.clone()));
    }
    if let Some(stderr) = child.stderr.take() {
        tokio::spawn(read_tagged(stderr, false, sender));
    }
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut total = 0usize;
    let mut output_open = true;
    let mut tick = tokio::time::interval(Duration::from_millis(25));
    loop {
        tokio::select! {
            chunk = chunks.recv(), if output_open => {
                if let Some((is_stdout, chunk)) = chunk {
                    total = total.saturating_add(chunk.len());
                    if total > OUTPUT_BYTES {
                        return Err(cleanup_error(stop_and_reap(&mut tree, &mut child).await));
                    }
                    if is_stdout { stdout.extend_from_slice(&chunk); } else { stderr.extend_from_slice(&chunk); }
                } else { output_open = false; }
            }
            _ = tick.tick() => {
                if Instant::now() >= deadline {
                    return Err(cleanup_error(stop_and_reap(&mut tree, &mut child).await));
                }
                match child.try_wait() {
                    Ok(Some(status)) => {
                        if !stop_and_reap(&mut tree, &mut child).await {
                            return Err(AuthorizationError::CleanupFailed);
                        }
                        let drain = async {
                            while let Some((is_stdout, chunk)) = chunks.recv().await {
                                total = total.saturating_add(chunk.len());
                                if total > OUTPUT_BYTES {
                                    return false;
                                }
                                if is_stdout {
                                    stdout.extend_from_slice(&chunk);
                                } else {
                                    stderr.extend_from_slice(&chunk);
                                }
                            }
                            true
                        };
                        if !tokio::time::timeout(Duration::from_millis(250), drain)
                            .await
                            .unwrap_or(false)
                        {
                            return Err(AuthorizationError::Failed);
                        }
                        return Ok(CommandOutput { success: status.success(), stdout, stderr });
                    }
                    Ok(None) => {}
                    Err(_) => {
                        return Err(cleanup_error(stop_and_reap(&mut tree, &mut child).await));
                    }
                }
            }
        }
    }
}

fn cleanup_error(stopped: bool) -> AuthorizationError {
    if stopped {
        AuthorizationError::Failed
    } else {
        AuthorizationError::CleanupFailed
    }
}

async fn read_tagged(
    mut input: impl AsyncRead + Unpin,
    stdout: bool,
    sender: tokio::sync::mpsc::Sender<(bool, Vec<u8>)>,
) {
    let mut buffer = [0_u8; 4096];
    loop {
        match input.read(&mut buffer).await {
            Ok(0) | Err(_) => return,
            Ok(read)
                if sender
                    .send((stdout, buffer[..read].to_vec()))
                    .await
                    .is_err() =>
            {
                return
            }
            Ok(_) => {}
        }
    }
}

async fn strict_status(inner: &Inner, provider: Cli) -> ConnectionState {
    let Some(program) = provider.locate() else {
        return ConnectionState::Unavailable;
    };
    let args: &[&str] = match provider {
        Cli::Claude => &["auth", "status"],
        Cli::Codex => &["login", "status"],
    };
    let output = match run_bounded(&program, args).await {
        Ok(output) => output,
        Err(AuthorizationError::CleanupFailed) => {
            inner.state.lock().expect("authorization state").blocked = true;
            return ConnectionState::Unavailable;
        }
        Err(_) => return ConnectionState::Unavailable,
    };
    match provider {
        Cli::Claude => status_from_output(output.success, &output.stdout),
        Cli::Codex => codex_status_from_output(output.success, &output.stderr),
    }
}

fn codex_status_from_output(success: bool, output: &[u8]) -> ConnectionState {
    let Ok(output) = std::str::from_utf8(output) else {
        return ConnectionState::Unavailable;
    };
    let output = output.trim();
    if success && output == "Logged in using ChatGPT" {
        ConnectionState::Ready
    } else if output == "Not logged in" || output.starts_with("Logged in using an API key") {
        ConnectionState::NeedsAuthorization
    } else {
        ConnectionState::Unavailable
    }
}

fn status_from_output(success: bool, output: &[u8]) -> ConnectionState {
    let Ok(value) = serde_json::from_slice::<Value>(output) else {
        return ConnectionState::Unavailable;
    };
    match (
        success,
        value["loggedIn"].as_bool(),
        value["authMethod"].as_str(),
    ) {
        (true, Some(true), Some("claude.ai")) => ConnectionState::Ready,
        (_, Some(false), _) | (true, Some(true), _) => ConnectionState::NeedsAuthorization,
        _ => ConnectionState::Unavailable,
    }
}

fn authorization_url(output: &[u8]) -> Option<String> {
    let start = output
        .windows(b"https://".len())
        .position(|window| window == b"https://")?;
    let tail = &output[start..];
    let end = tail
        .iter()
        .position(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
        .unwrap_or(tail.len());
    if end == 0 || end > URL_BYTES {
        return None;
    }
    let candidate = std::str::from_utf8(&tail[..end]).ok()?;
    validate_authorization_url(candidate).then(|| candidate.to_string())
}

fn validate_authorization_url(candidate: &str) -> bool {
    let Ok(url) = Url::parse(candidate) else {
        return false;
    };
    if url.scheme() != "https"
        || url.host_str() != Some("claude.com")
        || url.path() != "/cai/oauth/authorize"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.fragment().is_some()
    {
        return false;
    }
    let Some(raw_query) = url.query() else {
        return false;
    };
    let mut raw_redirects = Vec::new();
    let mut values: HashMap<String, Vec<String>> = HashMap::new();
    for part in raw_query.split('&') {
        let Some((raw_name, raw_value)) = part.split_once('=') else {
            return false;
        };
        if !query_component_valid(raw_name) || !query_component_valid(raw_value) {
            return false;
        }
        let Ok(decoded) = Url::parse(&format!("https://x.invalid/?{raw_name}={raw_value}")) else {
            return false;
        };
        let Some((name, value)) = decoded.query_pairs().next() else {
            return false;
        };
        if raw_name != name {
            return false;
        }
        if ![
            "client_id",
            "code",
            "code_challenge",
            "code_challenge_method",
            "redirect_uri",
            "response_type",
            "scope",
            "state",
        ]
        .contains(&name.as_ref())
            || value.len() > 4096
        {
            return false;
        }
        if name == "redirect_uri" {
            raw_redirects.push(raw_value);
        }
        values
            .entry(name.into_owned())
            .or_default()
            .push(value.into_owned());
    }
    for name in [
        "client_id",
        "code",
        "code_challenge",
        "code_challenge_method",
        "response_type",
        "scope",
        "state",
    ] {
        if values.get(name).is_none_or(|entries| entries.len() != 1) {
            return false;
        }
    }
    let redirects = values.get("redirect_uri");
    let Some(redirects) = redirects.filter(|entries| !entries.is_empty()) else {
        return false;
    };
    if raw_redirects.iter().any(|raw| *raw != raw_redirects[0])
        || redirects
            .iter()
            .any(|value| value != "https://platform.claude.com/oauth/code/callback")
    {
        return false;
    }
    !values["client_id"][0].is_empty()
        && !values["scope"][0].is_empty()
        && !values["state"][0].is_empty()
        && !values["code_challenge"][0].is_empty()
        && values["code"][0] == "true"
        && values["code_challenge_method"][0] == "S256"
        && values["response_type"][0] == "code"
}

fn query_component_valid(component: &str) -> bool {
    let bytes = component.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len()
                || !bytes[index + 1].is_ascii_hexdigit()
                || !bytes[index + 2].is_ascii_hexdigit()
            {
                return false;
            }
            index += 3;
        } else {
            index += 1;
        }
    }
    true
}

fn authorization_code_valid(code: &str) -> bool {
    !code.is_empty()
        && code.len() <= 4096
        && !code
            .as_bytes()
            .iter()
            .any(|byte| *byte <= 0x1f || *byte == 0x7f)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    static ENVIRONMENT: AsyncMutex<()> = AsyncMutex::const_new(());

    const URL: &str = "https://claude.com/cai/oauth/authorize?client_id=client&code=true&code_challenge=challenge&code_challenge_method=S256&redirect_uri=https%3A%2F%2Fplatform.claude.com%2Foauth%2Fcode%2Fcallback&redirect_uri=https%3A%2F%2Fplatform.claude.com%2Foauth%2Fcode%2Fcallback&response_type=code&scope=account%3Aread&state=state";

    #[cfg(unix)]
    struct FakeCli {
        _directory: tempfile::TempDir,
        previous: Option<std::ffi::OsString>,
    }

    #[cfg(unix)]
    struct FakeCodex {
        _directory: tempfile::TempDir,
        previous: Option<std::ffi::OsString>,
    }

    #[cfg(unix)]
    impl FakeCli {
        fn new(source: &str) -> Self {
            use std::os::unix::fs::PermissionsExt;

            let directory = tempfile::tempdir().expect("temporary directory");
            let script = directory.path().join("claude");
            std::fs::write(&script, source).expect("fake CLI");
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700))
                .expect("executable fake CLI");
            let previous = std::env::var_os("ARK_CLAUDE_BIN");
            std::env::set_var("ARK_CLAUDE_BIN", &script);
            Self {
                _directory: directory,
                previous,
            }
        }
    }

    #[cfg(unix)]
    impl Drop for FakeCli {
        fn drop(&mut self) {
            if let Some(previous) = self.previous.take() {
                std::env::set_var("ARK_CLAUDE_BIN", previous);
            } else {
                std::env::remove_var("ARK_CLAUDE_BIN");
            }
        }
    }

    #[cfg(unix)]
    impl FakeCodex {
        fn new(source: &str) -> Self {
            use std::os::unix::fs::PermissionsExt;

            let directory = tempfile::tempdir().expect("temporary directory");
            let script = directory.path().join("codex");
            std::fs::write(&script, source).expect("fake Codex CLI");
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700))
                .expect("executable fake Codex CLI");
            let previous = std::env::var_os("ARK_CODEX_BIN");
            std::env::set_var("ARK_CODEX_BIN", &script);
            Self {
                _directory: directory,
                previous,
            }
        }
    }

    #[cfg(unix)]
    impl Drop for FakeCodex {
        fn drop(&mut self) {
            if let Some(previous) = self.previous.take() {
                std::env::set_var("ARK_CODEX_BIN", previous);
            } else {
                std::env::remove_var("ARK_CODEX_BIN");
            }
        }
    }

    #[test]
    fn official_url_is_preserved_and_parameter_pollution_is_refused() {
        assert_eq!(
            authorization_url(format!("open {URL}\n").as_bytes()),
            Some(URL.into())
        );
        assert!(!validate_authorization_url(
            &URL.replace("state=state", "state=one&state=two")
        ));
        assert!(!validate_authorization_url(
            &URL.replace("scope=account%3Aread", "scope=x&extra=y")
        ));
        assert!(!validate_authorization_url(&URL.replace(
            "redirect_uri=https%3A%2F%2Fplatform.claude.com%2Foauth%2Fcode%2Fcallback&redirect_uri=https%3A%2F%2Fplatform.claude.com%2Foauth%2Fcode%2Fcallback",
            "redirect_uri=https%3A%2F%2Fplatform.claude.com%2Foauth%2Fcode%2Fcallback&redirect_uri=https://platform.claude.com/oauth/code/callback"
        )));
        assert!(!validate_authorization_url(
            &URL.replace("claude.com", "claude.com:443")
        ));
        assert!(!validate_authorization_url(&format!("{URL}#code")));
    }

    #[test]
    fn codes_are_bounded_and_reject_every_ascii_control() {
        assert!(authorization_code_valid("code 123"));
        assert!(!authorization_code_valid(""));
        assert!(!authorization_code_valid(&"a".repeat(4097)));
        for control in (0_u8..=0x1f).chain(std::iter::once(0x7f)) {
            let value = String::from_utf8(vec![b'a', control]).expect("ASCII is UTF-8");
            assert!(!authorization_code_valid(&value));
        }
    }

    #[test]
    fn status_requires_a_successful_claude_account_login() {
        assert_eq!(
            status_from_output(true, br#"{"loggedIn":true,"authMethod":"claude.ai"}"#),
            ConnectionState::Ready
        );
        assert_eq!(
            status_from_output(true, br#"{"loggedIn":true,"authMethod":"apiKey"}"#),
            ConnectionState::NeedsAuthorization
        );
        assert_eq!(
            status_from_output(false, br#"{"loggedIn":true,"authMethod":"claude.ai"}"#),
            ConnectionState::Unavailable
        );
        assert_eq!(
            status_from_output(false, br#"{"loggedIn":false}"#),
            ConnectionState::NeedsAuthorization
        );
    }

    #[test]
    fn codex_device_prompt_is_exact_bounded_and_redacted() {
        let prompt = b"Open\n\x1b[34mhttps://auth.openai.com/codex/device\x1b[0m\n2. Enter this one-time code (expires in 15 minutes)\n\x1b[34mABCDE-12-XYZ\x1b[0m\n";
        assert_eq!(
            codex_device_prompt(prompt),
            Some((
                "https://auth.openai.com/codex/device".into(),
                "ABCDE-12-XYZ".into()
            ))
        );
        assert!(codex_device_prompt(b"https://evil.example/codex/device\nABCD-EFGH\n").is_none());
        assert!(
            codex_device_prompt(b"https://auth.openai.com/codex/device\n2. Enter this one-time code (expires in 15 minutes)\ncode with spaces\n")
                .is_none()
        );
        assert!(!codex_user_code_valid(&"A".repeat(65)));
    }

    #[test]
    fn codex_status_accepts_only_exact_chatgpt_login() {
        assert_eq!(
            codex_status_from_output(true, b"Logged in using ChatGPT\n"),
            ConnectionState::Ready
        );
        assert_eq!(
            codex_status_from_output(true, b"Logged in using an API key - sk-...\n"),
            ConnectionState::NeedsAuthorization
        );
        assert_eq!(
            codex_status_from_output(true, b"Logged in using ChatGPT extra\n"),
            ConnectionState::Unavailable
        );
        assert_eq!(
            codex_status_from_output(false, b"Logged in using ChatGPT\n"),
            ConnectionState::Unavailable
        );
    }

    #[test]
    fn debug_output_redacts_the_authorization_url() {
        let view = AttemptView {
            id: "attempt".into(),
            connection_id: "claude".into(),
            state: AttemptState::WaitingForCode,
            authorization_url: Some(URL.into()),
            user_code: Some("SECRET-CODE".into()),
            expires_unix_seconds: 1,
            display: "Continue".into(),
        };
        let debug = format!("{view:?}");
        assert!(debug.contains("[redacted]"));
        assert!(!debug.contains("code_challenge"));
        assert!(!debug.contains("SECRET-CODE"));
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn official_cli_flow_is_global_idempotent_private_and_disconnects() {
        use std::os::unix::fs::PermissionsExt;

        let _environment = ENVIRONMENT.lock().await;
        let directory = tempfile::tempdir().expect("temporary directory");
        let script = directory.path().join("claude");
        let marker = directory.path().join("ready");
        let arguments = directory.path().join("arguments");
        let accepted = directory.path().join("accepted");
        let environment = directory.path().join("environment");
        let source = format!(
            "#!/bin/sh\n\
             printf '%s\\n' \"$*\" >> '{}'\n\
             case \"$*\" in\n\
             'auth status')\n\
               if [ -f '{}' ]; then printf '%s\\n' '{{\"loggedIn\":true,\"authMethod\":\"claude.ai\"}}'; else printf '%s\\n' '{{\"loggedIn\":false}}'; exit 1; fi ;;\n\
             'auth login --claudeai')\n\
               env | sort > '{}'\n\
               printf '%s\\n' '{}'\n\
               IFS= read -r code\n\
               printf '%s' \"$code\" > '{}'\n\
               [ \"$code\" = 'good code' ] && : > '{}' ;;\n\
             'auth logout') rm -f '{}' ;;\n\
             *) exit 2 ;;\n\
             esac\n",
            arguments.display(),
            marker.display(),
            environment.display(),
            URL,
            accepted.display(),
            marker.display(),
            marker.display(),
        );
        std::fs::write(&script, source).expect("fake CLI");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700))
            .expect("executable fake CLI");
        std::env::set_var("ARK_CLAUDE_BIN", &script);
        std::env::set_var("ANTHROPIC_API_KEY", "must-not-pass");
        std::env::set_var("HTTPS_PROXY", "https://secret@proxy.invalid");

        let authorization = ClaudeAuthorization::new();
        let first = authorization
            .begin("route", "owner-a", "listener", "request-a")
            .await
            .expect("begin");
        assert_eq!(first.state, AttemptState::WaitingForCode);
        assert_eq!(first.authorization_url.as_deref(), Some(URL));
        let repeated = authorization
            .begin("route", "owner-a", "listener", "request-a")
            .await
            .expect("idempotent begin");
        assert_eq!(repeated.id, first.id);
        assert_eq!(
            authorization
                .begin("route", "owner-b", "listener", "request-b")
                .await,
            Err(AuthorizationError::InProgress)
        );
        assert_eq!(
            authorization.attempt("route", "owner-b", "listener", &first.id),
            Err(AuthorizationError::Missing)
        );
        assert_eq!(
            authorization.attempt("other-route", "owner-a", "listener", &first.id),
            Err(AuthorizationError::Missing)
        );
        assert_eq!(
            authorization.attempt("route", "owner-a", "replacement-listener", &first.id),
            Err(AuthorizationError::Missing)
        );
        assert_eq!(
            authorization.submit(
                "route",
                "owner-a",
                "replacement-listener",
                &first.id,
                "good code",
            ),
            Err(AuthorizationError::Missing)
        );
        assert_eq!(
            authorization
                .cancel("route", "owner-a", "replacement-listener", &first.id)
                .await,
            Err(AuthorizationError::Missing)
        );
        assert_eq!(
            authorization
                .begin("route", "owner-a", "replacement-listener", "request-a",)
                .await,
            Err(AuthorizationError::InProgress)
        );
        assert_eq!(
            authorization
                .submit("route", "owner-a", "listener", &first.id, "good code")
                .expect("submit")
                .state,
            AttemptState::Verifying
        );
        assert_eq!(
            authorization.submit("route", "owner-a", "listener", &first.id, "good code"),
            Err(AuthorizationError::CodeUsed)
        );
        let ready = loop {
            let attempt = authorization
                .attempt("route", "owner-a", "listener", &first.id)
                .expect("retained attempt");
            if attempt.state == AttemptState::Ready {
                break attempt;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        assert_eq!(ready.id, first.id);
        assert_eq!(
            authorization
                .begin("route", "owner-a", "listener", "request-a")
                .await
                .expect("terminal retry")
                .state,
            AttemptState::Ready
        );
        assert_eq!(
            authorization.disconnect().await.expect("disconnect"),
            ConnectionState::NeedsAuthorization
        );

        assert_eq!(std::fs::read_to_string(&accepted).unwrap(), "good code");
        let child_environment = std::fs::read_to_string(&environment).unwrap();
        assert!(!child_environment.contains("ANTHROPIC_API_KEY"));
        assert!(!child_environment.contains("HTTPS_PROXY"));
        assert!(child_environment.contains("DISABLE_UPDATES=1"));
        assert_eq!(
            std::fs::read_to_string(&arguments).unwrap(),
            "auth status\nauth login --claudeai\nauth status\nauth status\nauth logout\nauth status\n"
        );

        std::env::remove_var("HTTPS_PROXY");
        std::env::remove_var("ANTHROPIC_API_KEY");
        std::env::remove_var("ARK_CLAUDE_BIN");
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn codex_device_flow_is_exact_private_and_global_across_providers() {
        let _environment = ENVIRONMENT.lock().await;
        let directory = tempfile::tempdir().expect("state directory");
        let ready = directory.path().join("ready");
        let approved = directory.path().join("approved");
        let arguments = directory.path().join("arguments");
        let environment = directory.path().join("environment");
        let source = format!(
            "#!/bin/sh\n\
             printf '%s\\n' \"$*\" >> '{}'\n\
             case \"$*\" in\n\
             'login status')\n\
               if [ -f '{}' ]; then printf '%s\\n' 'Logged in using ChatGPT' >&2; else printf '%s\\n' 'Not logged in' >&2; exit 1; fi ;;\n\
             'login --device-auth')\n\
               env | sort > '{}'\n\
               printf '%s\\n' 'Open this URL' 'https://auth.openai.com/codex/device' '2. Enter this one-time code (expires in 15 minutes)' 'ABCDE-12-XYZ' >&2\n\
               while [ ! -f '{}' ]; do sleep 0.01; done\n\
               : > '{}' ;;\n\
             'logout') rm -f '{}' ;;\n\
             *) exit 2 ;;\n\
             esac\n",
            arguments.display(),
            ready.display(),
            environment.display(),
            approved.display(),
            ready.display(),
            ready.display(),
        );
        let _cli = FakeCodex::new(&source);
        std::env::set_var("OPENAI_API_KEY", "must-not-pass");
        std::env::set_var("CODEX_ACCESS_TOKEN", "must-not-pass");
        std::env::set_var("OPENAI_BASE_URL", "https://secret.invalid");
        std::env::set_var("HTTPS_PROXY", "https://secret@proxy.invalid");

        let authorization = ClaudeAuthorization::new();
        let attempt = authorization
            .begin_for(Cli::Codex, "route", "owner", "listener", "codex-request")
            .await
            .expect("Codex device flow starts");
        assert_eq!(attempt.connection_id, "codex");
        assert_eq!(attempt.state, AttemptState::WaitingForBrowser);
        assert_eq!(
            attempt.authorization_url.as_deref(),
            Some("https://auth.openai.com/codex/device")
        );
        assert_eq!(attempt.user_code.as_deref(), Some("ABCDE-12-XYZ"));
        assert_eq!(
            authorization
                .begin("route", "owner", "listener", "claude-request")
                .await,
            Err(AuthorizationError::InProgress)
        );
        assert_eq!(
            authorization.attempt("route", "other-owner", "listener", &attempt.id),
            Err(AuthorizationError::Missing)
        );

        std::fs::write(&approved, b"").expect("approve fake device flow");
        loop {
            let current = authorization
                .attempt("route", "owner", "listener", &attempt.id)
                .expect("owned attempt remains visible");
            if current.state != AttemptState::WaitingForBrowser {
                assert_eq!(current.state, AttemptState::Ready);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            authorization
                .disconnect_for(Cli::Codex)
                .await
                .expect("official logout"),
            ConnectionState::NeedsAuthorization
        );

        let child_environment = std::fs::read_to_string(&environment).unwrap();
        for secret in [
            "OPENAI_API_KEY",
            "CODEX_ACCESS_TOKEN",
            "OPENAI_BASE_URL",
            "HTTPS_PROXY",
        ] {
            assert!(!child_environment.contains(secret), "leaked {secret}");
            std::env::remove_var(secret);
        }
        assert_eq!(
            std::fs::read_to_string(&arguments).unwrap(),
            "login status\nlogin --device-auth\nlogin status\nlogin status\nlogout\nlogin status\n"
        );
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn cancelling_codex_device_flow_reaps_its_descendants() {
        let _environment = ENVIRONMENT.lock().await;
        let directory = tempfile::tempdir().expect("state directory");
        let descendant = directory.path().join("descendant");
        let _cli = FakeCodex::new(&format!(
            "#!/bin/sh\ncase \"$*\" in\n\
             'login status') echo 'Not logged in' >&2; exit 1 ;;\n\
             'login --device-auth') sleep 30 & echo $! > '{}'; printf '%s\\n' 'https://auth.openai.com/codex/device' '2. Enter this one-time code (expires in 15 minutes)' 'ABCDE-12-XYZ' >&2; wait ;;\n\
             *) exit 2 ;; esac\n",
            descendant.display(),
        ));
        let authorization = ClaudeAuthorization::new();
        let attempt = authorization
            .begin_for(Cli::Codex, "route", "owner", "listener", "request")
            .await
            .expect("Codex device flow starts");
        let pid: u32 = loop {
            if let Ok(pid) = std::fs::read_to_string(&descendant) {
                break pid.trim().parse().unwrap();
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        let cancelled = authorization
            .cancel("route", "owner", "listener", &attempt.id)
            .await
            .expect("cancelled after verified cleanup");
        assert_eq!(cancelled.state, AttemptState::Cancelled);
        assert!(!ark_os::process::alive(pid));
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn a_failed_login_process_cannot_reuse_preexisting_credentials() {
        let _environment = ENVIRONMENT.lock().await;
        let directory = tempfile::tempdir().expect("state directory");
        let ready = directory.path().join("ready");
        let _cli = FakeCli::new(&format!(
            "#!/bin/sh\ncase \"$*\" in\n\
             'auth status') if [ -f '{}' ]; then echo '{{\"loggedIn\":true,\"authMethod\":\"claude.ai\"}}'; else echo '{{\"loggedIn\":false}}'; exit 1; fi ;;\n\
             'auth login --claudeai') echo '{}'; IFS= read -r code; : > '{}'; exit 1 ;;\n\
             *) exit 2 ;; esac\n",
            ready.display(),
            URL,
            ready.display(),
        ));
        let authorization = ClaudeAuthorization::new();
        let attempt = authorization
            .begin("route", "owner", "listener", "request")
            .await
            .unwrap();
        authorization
            .submit("route", "owner", "listener", &attempt.id, "code")
            .unwrap();
        loop {
            let attempt = authorization
                .attempt("route", "owner", "listener", &attempt.id)
                .unwrap();
            if attempt.state != AttemptState::Verifying {
                assert_eq!(attempt.state, AttemptState::Failed);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn cancelling_waits_for_the_process_group_before_another_login() {
        let _environment = ENVIRONMENT.lock().await;
        let directory = tempfile::tempdir().expect("state directory");
        let descendant = directory.path().join("descendant");
        let _cli = FakeCli::new(&format!(
            "#!/bin/sh\ncase \"$*\" in\n\
             'auth status') echo '{{\"loggedIn\":false}}'; exit 1 ;;\n\
             'auth login --claudeai') sleep 30 & echo $! > '{}'; echo '{}'; IFS= read -r code ;;\n\
             *) exit 2 ;; esac\n",
            descendant.display(),
            URL,
        ));
        let authorization = ClaudeAuthorization::new();
        let first = authorization
            .begin("route", "owner", "listener", "request-1")
            .await
            .unwrap();
        let first_pid: u32 = loop {
            if let Ok(pid) = std::fs::read_to_string(&descendant) {
                break pid.trim().parse().unwrap();
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        let cancelled = authorization
            .cancel("route", "owner", "listener", &first.id)
            .await
            .unwrap();
        assert_eq!(cancelled.state, AttemptState::Cancelled);
        assert!(!ark_os::process::alive(first_pid));

        let second = authorization
            .begin("route", "owner", "listener", "request-2")
            .await
            .expect("the next login starts after cleanup");
        assert_ne!(second.id, first.id);
        authorization
            .cancel("route", "owner", "listener", &second.id)
            .await
            .unwrap();
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn revoking_the_owner_reaps_the_login_process_group_before_reuse() {
        let _environment = ENVIRONMENT.lock().await;
        let directory = tempfile::tempdir().expect("state directory");
        let descendant = directory.path().join("descendant");
        let _cli = FakeCli::new(&format!(
            "#!/bin/sh\ncase \"$*\" in\n\
             'auth status') echo '{{\"loggedIn\":false}}'; exit 1 ;;\n\
             'auth login --claudeai') sleep 30 & echo $! > '{}'; echo '{}'; IFS= read -r code ;;\n\
             *) exit 2 ;; esac\n",
            descendant.display(),
            URL,
        ));
        let authorization = ClaudeAuthorization::new();
        let attempt = authorization
            .begin("route", "owner", "listener", "request")
            .await
            .unwrap();
        let pid: u32 = loop {
            if let Ok(pid) = std::fs::read_to_string(&descendant) {
                break pid.trim().parse().unwrap();
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        authorization.cancel_if_owner_revoked("other-listener", |_| false);
        assert_eq!(
            authorization
                .attempt("route", "owner", "listener", &attempt.id)
                .unwrap()
                .state,
            AttemptState::WaitingForCode
        );
        authorization.cancel_if_owner_revoked("listener", |_| false);
        assert_eq!(
            authorization
                .attempt("route", "owner", "listener", &attempt.id)
                .unwrap()
                .state,
            AttemptState::Cancelled
        );
        loop {
            if authorization.connection().await != ConnectionState::Authorizing {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(!ark_os::process::alive(pid));
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn bounded_output_and_missing_url_fail_without_waiting_for_attempt_expiry() {
        let _environment = ENVIRONMENT.lock().await;
        let _cli = FakeCli::new(
            "#!/bin/sh\ncase \"$*\" in\n\
             'auth status') echo '{\"loggedIn\":false}'; exit 1 ;;\n\
             'auth login --claudeai') dd if=/dev/zero bs=1024 count=65 2>/dev/null | tr '\\000' x; sleep 30 ;;\n\
             *) exit 2 ;; esac\n",
        );
        let authorization = ClaudeAuthorization::new();
        let started = Instant::now();
        let attempt = authorization
            .begin("route", "owner", "listener", "request")
            .await
            .expect("failed attempt remains addressable");
        assert_eq!(attempt.state, AttemptState::Failed);
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn invalid_url_hits_the_start_deadline_and_reaps_descendants() {
        let _environment = ENVIRONMENT.lock().await;
        let directory = tempfile::tempdir().expect("state directory");
        let descendant = directory.path().join("descendant");
        let _cli = FakeCli::new(&format!(
            "#!/bin/sh\ncase \"$*\" in\n\
             'auth status') echo '{{\"loggedIn\":false}}'; exit 1 ;;\n\
             'auth login --claudeai') sleep 30 & echo $! > '{}'; echo 'https://example.com/cai/oauth/authorize'; IFS= read -r code ;;\n\
             *) exit 2 ;; esac\n",
            descendant.display(),
        ));
        let authorization = ClaudeAuthorization::new();
        let started = Instant::now();
        let attempt = authorization
            .begin("route", "owner", "listener", "request")
            .await
            .expect("failed attempt remains addressable");
        assert_eq!(attempt.state, AttemptState::Failed);
        assert!(started.elapsed() < Duration::from_secs(2));
        let pid: u32 = std::fs::read_to_string(descendant)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(!ark_os::process::alive(pid));
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn failed_cancel_delivery_cannot_release_an_unverified_process() {
        let _environment = ENVIRONMENT.lock().await;
        let _cli = FakeCli::new(&format!(
            "#!/bin/sh\ncase \"$*\" in\n\
             'auth status') echo '{{\"loggedIn\":false}}'; exit 1 ;;\n\
             'auth login --claudeai') echo '{}'; IFS= read -r code ;;\n\
             *) exit 2 ;; esac\n",
            URL,
        ));
        let authorization = ClaudeAuthorization::new();
        let attempt = authorization
            .begin("route", "owner", "listener", "request")
            .await
            .unwrap();
        authorization
            .inner
            .state
            .lock()
            .unwrap()
            .attempts
            .get_mut(&attempt.id)
            .unwrap()
            .cancel
            .take();
        assert_eq!(
            authorization
                .cancel("route", "owner", "listener", &attempt.id)
                .await,
            Err(AuthorizationError::Failed)
        );
        assert_eq!(
            authorization.connection().await,
            ConnectionState::Unavailable
        );
        {
            let mut state = authorization.inner.state.lock().unwrap();
            let retained = state.attempts.get_mut(&attempt.id).unwrap();
            let past = Instant::now() - ATTEMPT_TTL - TERMINAL_GRACE;
            retained.expires = past;
            retained.terminal_at = Some(past);
            prune(&mut state);
            assert!(state.attempts.contains_key(&attempt.id));
        }
        assert_eq!(
            authorization
                .begin("route", "owner", "listener", "second")
                .await,
            Err(AuthorizationError::InProgress)
        );
        tokio::time::timeout(Duration::from_secs(3), async {
            while authorization.connection().await == ConnectionState::Unavailable {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("only verified runner cleanup releases the reservation");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unverified_logout_cleanup_blocks_future_authorization() {
        let authorization = ClaudeAuthorization::new();
        authorization.inner.state.lock().unwrap().blocked = true;
        assert_eq!(
            authorization.connection().await,
            ConnectionState::Unavailable
        );
        assert_eq!(
            authorization
                .begin("route", "owner", "listener", "request")
                .await,
            Err(AuthorizationError::Unavailable)
        );
        assert_eq!(
            authorization.disconnect().await,
            Err(AuthorizationError::Unavailable)
        );
        assert_eq!(cleanup_error(false), AuthorizationError::CleanupFailed);
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn failed_code_delivery_keeps_the_global_reservation_until_cleanup() {
        let _environment = ENVIRONMENT.lock().await;
        let _cli = FakeCli::new(&format!(
            "#!/bin/sh\ncase \"$*\" in\n\
             'auth status') echo '{{\"loggedIn\":false}}'; exit 1 ;;\n\
             'auth login --claudeai') echo '{}'; IFS= read -r code ;;\n\
             *) exit 2 ;; esac\n",
            URL,
        ));
        let authorization = ClaudeAuthorization::new();
        let attempt = authorization
            .begin("route", "owner", "listener", "request")
            .await
            .unwrap();
        let (sender, receiver) = oneshot::channel();
        drop(receiver);
        authorization
            .inner
            .state
            .lock()
            .unwrap()
            .attempts
            .get_mut(&attempt.id)
            .unwrap()
            .code = Some(sender);
        assert_eq!(
            authorization.submit("route", "owner", "listener", &attempt.id, "code"),
            Err(AuthorizationError::Failed)
        );
        assert_eq!(
            authorization.connection().await,
            ConnectionState::Unavailable
        );
        assert_eq!(
            authorization
                .begin("route", "owner", "listener", "second")
                .await,
            Err(AuthorizationError::InProgress)
        );
        tokio::time::timeout(Duration::from_secs(3), async {
            while authorization.connection().await == ConnectionState::Unavailable {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the runner cleans up the abandoned code channel");
    }
}
