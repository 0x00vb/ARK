//! # ark-ui
//!
//! A graphical agent workspace for any ARK app: the conversation, what the agent is doing,
//! approvals, sessions, and inline reports of context, working state, usage and traces. It opens
//! the same [`ark_interact::App`] the terminal shell does, so commands, sessions and traces mean
//! the same thing in both.
//!
//! ```ignore
//! use ark_ui::ArkUi;
//!
//! fn main() -> std::process::ExitCode {
//!     let app = my_app();                        // an ark_interact::App
//!     ArkUi::new(app)
//!         .accent("#3b82f6")
//!         .suggestion("Summarize the project", "What is in this project so far?")
//!         .asset("my-panels.js", include_bytes!("../ui/my-panels.js").as_slice())
//!         .script("my-panels.js")
//!         .run_desktop(tauri::generate_context!())
//! }
//! ```
//!
//! Two transports carry the same page and protocol: the native window (feature `desktop`,
//! Tauri) and an HTTP server ([`ArkUi::serve`]): local with a one-time token, or a runtime
//! other machines connect to with saved [`access::Tokens`] ([`remote`], ADR 0016). Neither holds agent logic; both
//! drive a [`Host`] over one `Interaction`. Consumers extend the page with their own scripts
//! (panels, artifact viewers, tool renderers, actions, navigation, inspector sections; see
//! `docs/UI.md`) and back them with [`ArkUi::handler`] methods.

pub mod access;
mod assets;
pub mod computers;
pub mod copy;
#[cfg(feature = "desktop")]
mod desktop;
mod files;
mod frame;
pub mod host;
mod notify;
pub mod push;
pub mod remote;
mod serve;
mod terminal;
mod upload;

use std::borrow::Cow;
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;

use ark_interact::{App, ChannelApprover, Start};
use serde_json::Value;

/// What an app's [`computers::ComputerSetup`] is made of.
pub use ark_reach;
pub use assets::Assets;
pub use host::{Emitter, Handler, HandlerCtx, Host, Theme, UiInfo, Welcome};
pub use serve::{Access, Served};

/// Builds the workspace for an app. See the crate docs.
pub struct ArkUi {
    app: App,
    info: UiInfo,
    files: HashMap<String, Cow<'static, [u8]>>,
    handlers: HashMap<String, Handler>,
    start: Start,
    access: Access,
    entry: Option<String>,
    keep_running: bool,
    links: Vec<ark_interact::links::Link>,
    computers: Option<computers::ComputerSetup>,
    keep_computers: Vec<(String, String, String)>,
}

impl ArkUi {
    /// Show what waits on another runtime too, beside this one's (ADR 0043 step 4): a person's
    /// own server, reached at `url` with `token`, called `name` ("My machine"). Its items are
    /// answered there. Whoever reaches this runtime acts on that one as this runtime's owner:
    /// link only runtimes of the same person. `Host::links()` changes them later.
    pub fn link(mut self, name: &str, url: &str, token: &str) -> Self {
        self.links
            .push(ark_interact::links::Link::new(name, url, token));
        self
    }

    /// Let the person add their own computers over SSH (ADR 0052): a chat can then run on one.
    /// `setup` says what to install there (the app's program, a build per system) and where
    /// tokens are kept.
    pub fn computers(mut self, setup: computers::ComputerSetup) -> Self {
        self.computers = Some(setup);
        self
    }

    /// A computer the app kept before, reached at `url` with `token` (ADR 0052: today's "my
    /// own server" becomes a computer by address). Kept in the store and the app's secrets from
    /// now on, so the app can forget its own copy.
    pub fn keep_computer(mut self, name: &str, url: &str, token: &str) -> Self {
        self.keep_computers
            .push((name.to_string(), url.to_string(), token.to_string()));
        self
    }

    pub fn new(app: App) -> Self {
        ArkUi {
            app,
            info: UiInfo::default(),
            files: HashMap::new(),
            handlers: HashMap::new(),
            start: Start::default(),
            access: Access::default(),
            entry: None,
            keep_running: false,
            links: Vec::new(),
            computers: None,
            keep_computers: Vec::new(),
        }
    }
    /// The accent colour, any CSS colour.
    pub fn accent(mut self, css: impl Into<String>) -> Self {
        self.info.accent = Some(css.into());
        self
    }
    /// The colours the window opens with until the person chooses in Settings › Appearance
    /// (default: follow the system).
    pub fn theme(mut self, theme: Theme) -> Self {
        self.info.theme = theme;
        self
    }
    /// A logo image among the app's assets (`.asset("logo.svg", …)` then `.logo("logo.svg")`).
    pub fn logo(mut self, asset: impl Into<String>) -> Self {
        self.info.logo = Some(format!("ext/{}", asset.into()));
        self
    }
    /// The heading and line on an empty session.
    pub fn welcome(mut self, title: impl Into<String>, subtitle: impl Into<String>) -> Self {
        self.info.welcome.title = Some(title.into());
        self.info.welcome.subtitle = Some(subtitle.into());
        self
    }
    /// A starting point on an empty session: a short label and the prompt it sends.
    pub fn suggestion(mut self, label: impl Into<String>, prompt: impl Into<String>) -> Self {
        self.info
            .welcome
            .suggestions
            .push((label.into(), prompt.into()));
        self
    }
    /// A file the page can load, served at `ext/<path>`.
    pub fn asset(mut self, path: impl Into<String>, bytes: impl Into<Cow<'static, [u8]>>) -> Self {
        self.files.insert(path.into(), bytes.into());
        self
    }
    /// Load one of the app's assets as a script module after ARK's own.
    pub fn script(mut self, asset: impl Into<String>) -> Self {
        self.info.scripts.push(asset.into());
        self
    }
    /// Load one of the app's assets as a stylesheet after ARK's own.
    pub fn stylesheet(mut self, asset: impl Into<String>) -> Self {
        self.info.styles.push(asset.into());
        self
    }
    /// A backend method for the app's scripts: `ark.ext(name, params)`.
    pub fn handler<F, Fut>(mut self, name: impl Into<String>, f: F) -> Self
    where
        F: Fn(HandlerCtx, Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = anyhow::Result<Value>> + Send + 'static,
    {
        let f = Arc::new(f);
        self.handlers
            .insert(name.into(), Arc::new(move |ctx, v| Box::pin(f(ctx, v))));
        self
    }
    /// Model, project and session choices, as the shell's flags give them.
    pub fn start(mut self, start: Start) -> Self {
        self.start = start;
        self
    }

    /// Who may call [`serve`](Self::serve): the one-time token (default, this machine only) or
    /// saved [`access::Tokens`] for a runtime other machines connect to (ADR 0016).
    pub fn access(mut self, access: Access) -> Self {
        self.access = access;
        self
    }

    /// Where a browser without a session is sent: the app's page that signs a person in and
    /// comes back with a one-time link (ADR 0040). With saved tokens only. Without it, the page
    /// says it needs a link.
    pub fn entry(mut self, url: impl Into<String>) -> Self {
        self.entry = Some(url.into());
        self
    }

    /// The native window: closing it hides it and the app keeps running, with an item in the
    /// menu bar (the tray elsewhere) to open it again or quit. Schedules and agents' work go on;
    /// a question that waits shows a system notification. Default: closing the window quits.
    pub fn keep_running(mut self, on: bool) -> Self {
        self.keep_running = on;
        self
    }

    /// Open the app and wrap it in a [`Host`]. Transports and tests start here.
    pub async fn open(self) -> anyhow::Result<(Arc<Host>, Arc<Assets>)> {
        let title = self.app.name().to_string();
        let (approver, approvals) = ChannelApprover::channel();
        let opened = self.app.open(&self.start, Arc::new(approver))?;
        // Unless the application installed its own logger.
        ark_interact::logging::init(ark_interact::logging::LogTarget::File(
            ark_interact::logging::file_in(&opened.interaction.ark().settings().store_root),
        ));
        let assets = Arc::new(Assets::new(
            title,
            self.files,
            self.info.scripts.clone(),
            self.info.styles.clone(),
            self.info.theme,
        ));
        let host = Host::new(opened.interaction, approvals, self.info, self.handlers).await;
        if !self.links.is_empty() {
            host.links().set(self.links);
        }
        if let Some(setup) = self.computers {
            host.use_computers(setup, self.keep_computers);
        }
        Ok((host, assets))
    }

    /// Serve the workspace at `addr` (e.g. `127.0.0.1:0`) until ctrl+c or SIGTERM. Prints the
    /// URL, which carries the one-time token unless [`access`](Self::access) gave saved tokens.
    ///
    /// `unix:/path/to.sock` serves on a Unix socket instead (saved tokens only, ADR 0052): the
    /// socket is mode 0600, its folder is made 0700 when missing and refused when others can
    /// write to it, a socket left by a stopped process is replaced, and one that answers means
    /// a server is already there. The socket file is removed when serving stops.
    pub async fn serve(self, addr: &str) -> anyhow::Result<()> {
        let (access, entry) = (self.access.clone(), self.entry.clone());
        let (host, assets) = self.open().await?;
        screen_stays_here(&host, &access)?;
        if let Access::Tokens(tokens) = &access {
            if tokens.list()?.is_empty() {
                eprintln!(
                    "no access tokens in {} yet: create one, or every client is refused",
                    tokens.path().display()
                );
            }
        }
        let (served, mut task) = serve::serve(host.clone(), assets, addr, access, entry).await?;
        println!("{}", served.url);
        tokio::select! {
            _ = stop_signal() => {}
            _ = &mut task => return Ok(()),
        }
        // A deploy or a machine move: work under way continues in the next process (ADR 0038).
        let grace = ark_sdk::prelude::ark_runtime::cancel::stop_grace();
        eprintln!(
            "stopping: letting work reach a safe point (up to {}s)",
            grace.as_secs()
        );
        tokio::select! {
            settled = host.stop(grace) => if !settled {
                eprintln!("stopping: some work did not reach a safe point in time");
            },
            _ = stop_signal() => eprintln!("stopping now"),
        }
        // A Unix socket's file goes with the listener.
        task.abort();
        let _ = task.await;
        Ok(())
    }

    /// Start serving and return at once (tests, embedding). `addr` as in [`serve`](Self::serve).
    pub async fn serve_background(self, addr: &str) -> anyhow::Result<(Arc<Host>, Served)> {
        let (access, entry) = (self.access.clone(), self.entry.clone());
        let (host, assets) = self.open().await?;
        screen_stays_here(&host, &access)?;
        let (served, _task) = serve::serve(host.clone(), assets, addr, access, entry).await?;
        Ok((host, served))
    }

    /// Open the native window. `context` is the app's `tauri::generate_context!()`.
    #[cfg(feature = "desktop")]
    pub fn run_desktop(self, context: tauri::Context<tauri::Wry>) -> std::process::ExitCode {
        let keep_running = self.keep_running;
        let opened = tauri::async_runtime::block_on(self.open());
        match opened {
            Ok((host, assets)) => desktop::run(host, assets, context, keep_running),
            Err(e) => {
                eprintln!("error: {e:#}");
                std::process::ExitCode::FAILURE
            }
        }
    }
}

/// Resolves when the process is told to stop: ctrl+c, or SIGTERM (what a deploy, a container
/// platform or a service manager sends).
async fn stop_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// A runtime other machines connect to never offers this machine's screen: someone elsewhere
/// cannot watch an agent use it or take it back (ADR 0018). The app turns the computer off
/// when it serves with saved tokens.
fn screen_stays_here(host: &Host, access: &Access) -> anyhow::Result<()> {
    if matches!(access, Access::Tokens(_)) && host.uses_computer() {
        anyhow::bail!(
            "this app lets agents use the computer; a runtime other machines connect to must not (ArkBuilder::computer(false) when serving with tokens)"
        );
    }
    Ok(())
}
