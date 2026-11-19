//! What ARK can use, as a person sees it: Web, Files, Mail, Calendar, and any tool server the
//! person added. One list for both front-ends (`/connect` in a terminal, Settings › Capabilities
//! in a window). A capability is ready, one sign-in away, or unavailable with a reason.
//!
//! Connecting opens the provider's sign-in in the person's browser and finishes in the
//! background: the list shows it ready once the person has signed in. A change applies from
//! the next turn.

use std::time::Duration;

use ark_sdk::prelude::ark_computer::{self, ComputerService};
use ark_sdk::prelude::ark_connect::{Account, CapabilityStatus, Catalog, Connections, State};
use ark_sdk::prelude::{ApprovalDecision, ApprovalRequest, Approver, Ark, Engine};
use serde::Serialize;

use crate::report::{Cell, Report, Row, Table, Tone};

/// How long a sign-in may stay open in the browser.
pub const SIGN_IN_TIME: Duration = Duration::from_secs(300);

/// One row of the list.
#[derive(Clone, Debug, Serialize)]
pub struct Listed {
    #[serde(flatten)]
    pub status: CapabilityStatus,
    /// Added by the person as a tool server (advanced), not from ARK's catalog.
    pub added: bool,
    /// The person turns it on and off here (the Computer), instead of connecting an account.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub switch: bool,
}

/// What a front-end needs to list and connect capabilities without holding the [`Ark`] (a
/// window keeps this while a turn runs).
#[derive(Clone)]
pub struct Parts {
    pub engine: std::sync::Arc<Engine>,
    pub per_agent: Vec<std::sync::Arc<dyn ark_sdk::prelude::ark_runtime::per_agent::PerAgent>>,
    pub connections: Option<Connections>,
    /// The Computer, when the app offers it: the person turns it on and off.
    pub computer: Option<std::sync::Arc<ComputerService>>,
}

impl Parts {
    pub fn of(ark: &Ark) -> Parts {
        Parts {
            engine: ark.shared_engine(),
            per_agent: ark.per_agent().to_vec(),
            connections: ark.connections().cloned(),
            computer: ark.computer().cloned(),
        }
    }
}

/// Every capability this app can show, in the catalog's order, then the added tool servers.
pub fn list(ark: &Ark) -> Vec<Listed> {
    list_parts(&Parts::of(ark))
}

/// [`list`] from [`Parts`].
pub fn list_parts(p: &Parts) -> Vec<Listed> {
    list_parts_for(p, None)
}

pub fn list_for(ark: &Ark, project: &ark_sdk::prelude::ProjectId) -> Vec<Listed> {
    list_parts_for(&Parts::of(ark), Some(project))
}

pub fn list_parts_for(p: &Parts, project: Option<&ark_sdk::prelude::ProjectId>) -> Vec<Listed> {
    let registry = match project {
        Some(project) => ark_sdk::offered_tools_for(&p.engine, project, &p.per_agent),
        None => ark_sdk::offered_tools(&p.engine, &p.per_agent),
    };
    let tools: Vec<String> = registry
        .adapters()
        .iter()
        .map(|a| a.name().to_string())
        .collect();
    let mut rows: Vec<Listed> = match &p.connections {
        Some(c) => c.status(&tools),
        None => Catalog::builtin()
            .capabilities
            .into_iter()
            .filter(|c| c.provider.is_none() && c.tools.iter().any(|t| tools.contains(t)))
            .map(|c| CapabilityStatus {
                id: c.id,
                label: c.label,
                says: c.says,
                state: State::Ready,
                provider: None,
                account: None,
            })
            .collect(),
    }
    .into_iter()
    .map(|mut status| {
        let switch = status.id == ark_computer::ID && switchable(p.computer.as_ref(), &status.id);
        if switch && p.computer.as_ref().is_some_and(|c| !c.is_on()) {
            status.state = State::Off;
        }
        Listed {
            status,
            added: false,
            switch,
        }
    })
    .collect();
    for s in crate::plugins::list(&p.engine) {
        rows.push(Listed {
            status: CapabilityStatus {
                id: s.name.clone(),
                label: s.name.clone(),
                says: format!("{} tools from a server you added", s.tools.len()),
                state: if s.running {
                    State::Ready
                } else {
                    State::Unavailable(s.error.unwrap_or_else(|| "stopped".into()))
                },
                provider: None,
                account: None,
            },
            added: true,
            switch: false,
        });
    }
    rows
}

/// Turn a capability the app offers on or off (the Computer, ADR 0045). On asks the system
/// for its permission prompt at once, so the first action does not wait for it.
pub fn turn(
    computer: Option<&std::sync::Arc<ComputerService>>,
    capability: &str,
    on: bool,
) -> anyhow::Result<()> {
    let c = computer
        .filter(|c| c.switchable() && capability.trim().eq_ignore_ascii_case(ark_computer::ID))
        .ok_or_else(|| anyhow::anyhow!("`{capability}` is not one this app turns on and off"))?;
    c.set_on(on).map_err(|e| {
        anyhow::anyhow!(
            "the Computer could not be turned {}: {e}",
            if on { "on" } else { "off" }
        )
    })?;
    if on {
        c.ask_permission();
    }
    Ok(())
}

/// Whether `capability` is one the person turns on and off here.
pub fn switchable(computer: Option<&std::sync::Arc<ComputerService>>, capability: &str) -> bool {
    computer.is_some_and(|c| c.switchable())
        && capability.trim().eq_ignore_ascii_case(ark_computer::ID)
}

/// Words an agent's request to turn on the Computer: "Allow ARK to use the apps on this
/// computer", Allow or Not now. Already on, it passes without a question. Every other question goes on unchanged.
struct TurnOnComputer {
    inner: std::sync::Arc<dyn Approver>,
    computer: std::sync::Arc<ComputerService>,
    /// The app's own sources (accounts, installs). A turn asks them before the Computer, so a
    /// need one of them knows ("node to run the app") is theirs to word, never this one's.
    app: Option<std::sync::Arc<dyn ark_sdk::prelude::AcquisitionProvider>>,
}

/// `approver`, with the Computer's request worded when the app offers it with a switch.
pub(crate) fn wrap_computer(
    approver: std::sync::Arc<dyn Approver>,
    ark: &Ark,
) -> std::sync::Arc<dyn Approver> {
    match ark.computer().filter(|c| c.switchable()) {
        Some(c) => std::sync::Arc::new(TurnOnComputer {
            inner: approver,
            computer: c.clone(),
            app: ark.profile().acquisition.clone(),
        }),
        None => approver,
    }
}

#[async_trait::async_trait]
impl Approver for TurnOnComputer {
    async fn decide(&self, request: &ApprovalRequest) -> ApprovalDecision {
        let need = request.input["need"].as_str().unwrap_or("");
        let theirs = self.app.as_ref().is_some_and(|a| a.knows(need));
        if request.action != "acquire_capability"
            || theirs
            || !ark_computer::names_the_computer(need)
        {
            return self.inner.decide(request).await;
        }
        if self.computer.is_on() {
            return ApprovalDecision::AllowOnce;
        }
        let mut ask = request.clone();
        // The line says what the yes allows (its first word is the button).
        ask.preview = "Allow ARK to use the apps on this computer".into();
        ask.summary = String::new();
        match self.inner.decide(&ask).await {
            ApprovalDecision::Deny => ApprovalDecision::Deny,
            _ => ApprovalDecision::AllowOnce,
        }
    }
}

/// Start connecting a capability: returns the sign-in page to open, and finishes in the
/// background once the person signs in there.
pub async fn connect(conns: Option<&Connections>, capability: &str) -> anyhow::Result<String> {
    let conns = conns.ok_or_else(|| anyhow::anyhow!("this app connects no accounts"))?;
    if let Some(c) = conns.catalog().capability(capability) {
        if c.provider.is_none() {
            anyhow::bail!("{} needs no account", c.label);
        }
        if conns.account_for(c).is_some() {
            anyhow::bail!("{} is already connected", c.label);
        }
    }
    let connecting = conns.begin(capability).await?;
    let url = connecting.url().to_string();
    let label = conns
        .catalog()
        .capability(capability)
        .map(|c| c.label.clone())
        .unwrap_or_default();
    tokio::spawn(async move {
        match connecting.finish(SIGN_IN_TIME).await {
            Ok(a) => tracing::info!("{label} connected as {}", a.name),
            Err(e) => tracing::warn!("{label} not connected: {e}"),
        }
    });
    Ok(url)
}

/// Asks the person before an agent's `acquire_capability`, in capability words ("Connect
/// Google so ARK can send email"), and on a yes runs the sign-in in their browser and waits for
/// it. Every other question goes to the front-end's approver unchanged.
pub struct Connector {
    inner: std::sync::Arc<dyn Approver>,
    conns: Connections,
    open: Opener,
}

/// Shows the person a sign-in page. Answers whether it could.
pub type Opener = std::sync::Arc<dyn Fn(&str) -> bool + Send + Sync>;

impl Connector {
    pub fn new(inner: std::sync::Arc<dyn Approver>, conns: Connections) -> Self {
        Connector {
            inner,
            conns,
            open: std::sync::Arc::new(open_in_browser),
        }
    }

    /// Show sign-in pages another way (a test, a front-end that opens them itself).
    pub fn opener(mut self, open: Opener) -> Self {
        self.open = open;
        self
    }
}

#[async_trait::async_trait]
impl Approver for Connector {
    async fn decide(&self, request: &ApprovalRequest) -> ApprovalDecision {
        if request.action != "acquire_capability" {
            return self.inner.decide(request).await;
        }
        let need = request.input["need"].as_str().unwrap_or("");
        let Some(cap) = self.conns.capability_for(need).cloned() else {
            return self.inner.decide(request).await;
        };
        if self.conns.account_for(&cap).is_some() {
            return ApprovalDecision::AllowOnce;
        }
        let provider = cap
            .provider
            .as_deref()
            .and_then(|p| self.conns.catalog().provider(p))
            .map(|p| p.label.clone())
            .unwrap_or_default();
        let mut ask = request.clone();
        ask.summary = format!(
            "Connect {provider} so ARK can {}. You sign in on {provider}'s own page.",
            cap.says
        );
        ask.preview = format!("Connect {}", cap.label);
        if !self.inner.decide(&ask).await.allows() {
            return ApprovalDecision::Deny;
        }
        let connecting = match self.conns.begin(&cap.id).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("{} not connected: {e}", cap.label);
                return ApprovalDecision::Deny;
            }
        };
        if !(self.open)(connecting.url()) {
            tracing::warn!(
                "open this page to connect {}: {}",
                cap.label,
                connecting.url()
            );
        }
        match connecting.finish(SIGN_IN_TIME).await {
            Ok(_) => ApprovalDecision::AllowOnce,
            Err(e) => {
                tracing::warn!("{} not connected: {e}", cap.label);
                ApprovalDecision::Deny
            }
        }
    }
}

/// Disconnect the account behind a capability (or an account id): revoked and forgotten.
pub async fn disconnect(conns: Option<&Connections>, which: &str) -> anyhow::Result<Account> {
    let conns = conns.ok_or_else(|| anyhow::anyhow!("this app connects no accounts"))?;
    Ok(conns.disconnect(which).await?)
}

/// Open a page in the person's browser, on the machine ARK runs on. Best effort. The link
/// goes to the system whole (`ark_os::open`): an OAuth link keeps every `&`.
pub fn open_in_browser(url: &str) -> bool {
    ark_os::open::open(url).is_ok()
}

/// The list as a report: ✓ ready, ○ one sign-in away (with a Connect action), ! unavailable.
pub fn report(rows: &[Listed]) -> Report {
    if rows.is_empty() {
        return Report::new().note("This app gives its agents no capabilities yet.");
    }
    let mut t = Table::plain(3);
    for r in rows {
        let s = &r.status;
        let (mark, tone, detail, command) = match &s.state {
            State::Ready => (
                "✓",
                Tone::Success,
                match &s.account {
                    Some(a) => format!("{} · {a}", s.says),
                    None => s.says.clone(),
                },
                None,
            ),
            State::Connect => (
                "○",
                Tone::Muted,
                format!("{} · /connect {}", s.says, s.id),
                Some(format!("/connect {}", s.id)),
            ),
            State::Off => (
                "○",
                Tone::Muted,
                format!("{} · /connect {}", s.says, s.id),
                Some(format!("/connect {}", s.id)),
            ),
            State::Unavailable(why) => ("!", Tone::Warning, format!("{} · {why}", s.says), None),
        };
        t.push(Row {
            cells: vec![
                Cell::new(mark, tone),
                Cell::new(s.label.clone(), Tone::Normal),
                Cell::new(detail, Tone::Muted),
            ],
            tone: Tone::Normal,
            command: command.or_else(|| {
                (r.switch && s.state == State::Ready).then(|| format!("/disconnect {}", s.id))
            }),
        });
    }
    Report::new().table(t)
}
