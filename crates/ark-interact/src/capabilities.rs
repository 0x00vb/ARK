//! What ARK can use, as a person sees it: Web, Files, Mail, Calendar, and any tool server the
//! person added. One list for both front-ends (`/connect` in a terminal, Settings › Capabilities
//! in a window). A capability is ready, one sign-in away, or unavailable with a reason.
//!
//! Connecting opens the provider's sign-in in the person's browser and finishes in the
//! background: the list shows it ready once the person has signed in. A change applies from
//! the next turn.

use std::time::Duration;

use ark_sdk::prelude::ark_connect::{Account, CapabilityStatus, Catalog, Connections, State};
use ark_sdk::prelude::ark_tools::web::WebAdapter;
use ark_sdk::prelude::{Ark, Engine};
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
}

/// What a front-end needs to list and connect capabilities without holding the [`Ark`] (a
/// window keeps this while a turn runs).
#[derive(Clone)]
pub struct Parts {
    pub engine: std::sync::Arc<Engine>,
    pub web: Option<WebAdapter>,
    pub connections: Option<Connections>,
}

impl Parts {
    pub fn of(ark: &Ark) -> Parts {
        Parts {
            engine: ark.shared_engine(),
            web: ark.web_adapter().cloned(),
            connections: ark.connections().cloned(),
        }
    }
}

/// Every capability this app can show, in the catalog's order, then the added tool servers.
pub fn list(ark: &Ark) -> Vec<Listed> {
    list_parts(&Parts::of(ark))
}

/// [`list`] from [`Parts`].
pub fn list_parts(p: &Parts) -> Vec<Listed> {
    let tools: Vec<String> = ark_sdk::offered_tools(&p.engine, p.web.as_ref())
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
    .map(|status| Listed {
        status,
        added: false,
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
        });
    }
    rows
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

/// Disconnect the account behind a capability (or an account id): revoked and forgotten.
pub async fn disconnect(conns: Option<&Connections>, which: &str) -> anyhow::Result<Account> {
    let conns = conns.ok_or_else(|| anyhow::anyhow!("this app connects no accounts"))?;
    Ok(conns.disconnect(which).await?)
}

/// Open a page in the person's browser, on the machine ARK runs on. Best effort.
pub fn open_in_browser(url: &str) -> bool {
    let mut cmd = if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else if cfg!(windows) {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", ""]);
        c
    } else {
        std::process::Command::new("xdg-open")
    };
    cmd.arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
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
            State::Unavailable(why) => ("!", Tone::Warning, format!("{} · {why}", s.says), None),
        };
        t.push(Row {
            cells: vec![
                Cell::new(mark, tone),
                Cell::new(s.label.clone(), Tone::Normal),
                Cell::new(detail, Tone::Muted),
            ],
            tone: Tone::Normal,
            command,
        });
    }
    Report::new().table(t)
}
