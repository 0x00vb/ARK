//! # ark-ui
//!
//! A graphical agent workspace for any ARK app: the conversation, what the agent is doing,
//! approvals, sessions, and inspectors for context, working state, usage and traces. It opens
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
//! Tauri) and a local browser server ([`ArkUi::serve`]). Neither holds agent logic; both
//! drive a [`Host`] over one `Interaction`. Consumers extend the page with their own scripts
//! (panels, artifact viewers, tool renderers, actions, navigation, inspector sections; see
//! `docs/UI.md`) and back them with [`ArkUi::handler`] methods.

mod assets;
#[cfg(feature = "desktop")]
mod desktop;
pub mod host;
mod serve;

use std::borrow::Cow;
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;

use ark_interact::{App, ChannelApprover, Start};
use serde_json::Value;

pub use assets::Assets;
pub use host::{Emitter, Handler, HandlerCtx, Host, UiInfo, Welcome};
pub use serve::Served;

/// Builds the workspace for an app. See the crate docs.
pub struct ArkUi {
    app: App,
    info: UiInfo,
    files: HashMap<String, Cow<'static, [u8]>>,
    handlers: HashMap<String, Handler>,
    start: Start,
}

impl ArkUi {
    pub fn new(app: App) -> Self {
        ArkUi {
            app,
            info: UiInfo::default(),
            files: HashMap::new(),
            handlers: HashMap::new(),
            start: Start::default(),
        }
    }
    /// The accent colour, any CSS colour.
    pub fn accent(mut self, css: impl Into<String>) -> Self {
        self.info.accent = Some(css.into());
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
        ));
        let host = Host::new(opened.interaction, approvals, self.info, self.handlers).await;
        Ok((host, assets))
    }

    /// Serve the workspace to a local browser at `addr` (e.g. `127.0.0.1:0`) until ctrl+c.
    /// Prints the URL, which carries the access token.
    pub async fn serve(self, addr: &str) -> anyhow::Result<()> {
        let (host, assets) = self.open().await?;
        let (served, task) = serve::serve(host, assets, addr).await?;
        println!("{}", served.url);
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = task => {}
        }
        Ok(())
    }

    /// Start serving and return at once (tests, embedding).
    pub async fn serve_background(self, addr: &str) -> anyhow::Result<(Arc<Host>, Served)> {
        let (host, assets) = self.open().await?;
        let (served, _task) = serve::serve(host.clone(), assets, addr).await?;
        Ok((host, served))
    }

    /// Open the native window. `context` is the app's `tauri::generate_context!()`.
    #[cfg(feature = "desktop")]
    pub fn run_desktop(self, context: tauri::Context<tauri::Wry>) -> std::process::ExitCode {
        let opened = tauri::async_runtime::block_on(self.open());
        match opened {
            Ok((host, assets)) => desktop::run(host, assets, context),
            Err(e) => {
                eprintln!("error: {e:#}");
                std::process::ExitCode::FAILURE
            }
        }
    }
}
