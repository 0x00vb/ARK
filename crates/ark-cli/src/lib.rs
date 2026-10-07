//! # ark-cli
//!
//! The interactive terminal shell every ARK application enters by default. Launching the app
//! drops you into a persistent agent session; `-p` runs one prompt for scripts and CI. Both
//! paths drive the same runtime.
//!
//! ```ignore
//! use ark_cli::{ArkCli, Accent, Theme};
//! use ark_interact::App;
//!
//! #[tokio::main]
//! async fn main() -> std::process::ExitCode {
//!     let app = App::new("Writer")
//!         .tagline("a developmental editor for your novel")
//!         .ark(build_ark()?)                       // tools + AgentProfile + model
//!         .project(|ark| ark.find_or_create_project("my-novel", ""))
//!         .command(std::sync::Arc::new(ChapterCommand));
//!     ArkCli::new(app)
//!         .theme(Theme::default().accent(Accent::Magenta).prompt("✎ "))
//!         .run()
//!         .await
//! }
//! ```
//!
//! The app (agent, commands, labels, status) is an `ark_interact::App`; the same value can
//! open in `ark-ui` instead. This crate adds only the terminal: line editing, the activity
//! view, the approval menu, colour. The runtime never learns about the terminal.
//!
//! `--connect URL` opens the terminal on a runtime served elsewhere instead (ADR 0024): the
//! chat open there, its agents, its store and its models. Nothing runs in this process.

mod app;
mod approval;
mod chat;
mod driver;
mod headless;
mod markdown;
mod remote;
mod render;
mod shell;
mod term;
mod theme;
mod view;

use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Arc;

use ark_interact::{App, ChannelApprover, Start};

use ark_sdk::prelude::*;
use chat::{Chat, Local};

pub use app::{EventHook, Welcome};
pub use markdown::MdStream;
pub use render::render;
pub use term::Out;
pub use theme::{Accent, Glyphs, Theme};
// The shared interaction layer, so a CLI app can import everything from here.
pub use ark_interact::{
    self, ActivityLabels, Block, Command, Effect, Interaction, Reply, Report, Tone, ToolLabeler,
};

use app::{Config, ShellState};

/// The generic flags every ARK application accepts. Flatten into your own `clap` parser to
/// add domain subcommands:
///
/// ```ignore
/// #[derive(clap::Parser)]
/// struct Cli {
///     #[command(flatten)]
///     ark: ark_cli::ArkArgs,
///     #[command(subcommand)]
///     cmd: Option<MyCommands>,
/// }
/// ```
#[derive(clap::Args, Clone, Debug, Default)]
pub struct ArkArgs {
    /// Run one prompt without the interactive shell, print the answer, and exit
    #[arg(short = 'p', long = "print", value_name = "PROMPT")]
    pub print: Option<String>,
    /// With -p: print a single JSON result object
    #[arg(long, requires = "print")]
    pub json: bool,
    /// With -p: stream every runtime event as a JSON line
    #[arg(long, requires = "print", conflicts_with = "json")]
    pub events: bool,
    /// Continue the most recent session
    #[arg(short = 'r', long)]
    pub resume: bool,
    /// Continue a specific session (id or unique prefix)
    #[arg(long, value_name = "ID")]
    pub session: Option<String>,
    /// Work in this project (name or id; created if missing)
    #[arg(long, value_name = "NAME")]
    pub project: Option<String>,
    /// With -p: run state-changing actions without asking
    #[arg(long)]
    pub yes: bool,
    /// Show tool arguments, durations and model calls
    #[arg(short = 'v', long)]
    pub verbose: bool,
    /// Use this model: `<model>` or `<provider>:<model>` (e.g. anthropic:claude-sonnet-5-5)
    #[arg(long, value_name = "MODEL")]
    pub model: Option<String>,
    /// Use the ARK runtime served at this address (https://…) instead of this machine's
    #[arg(long, value_name = "URL", conflicts_with_all = ["resume", "session", "project", "model"])]
    pub connect: Option<String>,
    /// With --connect: the access token (default: the ARK_TOKEN environment variable)
    #[arg(long, value_name = "TOKEN", requires = "connect")]
    pub token: Option<String>,
}

/// The shell's keys, as `/help` lists them.
const KEYS: &[(&str, &str)] = &[
    ("enter", "send"),
    ("alt+enter, ctrl+j", "new line (or end a line with \\)"),
    ("↑ / ↓", "input history"),
    ("tab", "complete a /command"),
    ("esc, ctrl+c", "stop the agent (while it works)"),
    (
        "enter (while it works)",
        "send what you typed to the running turn",
    ),
    ("tab (while it works)", "send it after the turn instead"),
    ("ctrl+d, /exit", "leave (the session is saved)"),
];

/// Entry point. See the crate docs.
pub struct ArkCli {
    app: App,
    bin: Option<String>,
    theme: Theme,
    welcome: Option<app::WelcomeFn>,
    event_hook: Option<EventHook>,
    history_file: Option<PathBuf>,
}

impl ArkCli {
    /// The terminal front-end for `app`.
    pub fn new(app: App) -> Self {
        ArkCli {
            app,
            bin: None,
            theme: Theme::default(),
            welcome: None,
            event_hook: None,
            history_file: None,
        }
    }
    /// The executable name used in hints (`<bin> --resume`). Defaults to the running binary.
    pub fn bin(mut self, bin: impl Into<String>) -> Self {
        self.bin = Some(bin.into());
        self
    }
    pub fn theme(mut self, theme: Theme) -> Self {
        self.theme = theme;
        self
    }
    /// Shorthand for `theme(theme.prompt(symbol))`.
    pub fn prompt(mut self, symbol: impl Into<String>) -> Self {
        self.theme = self.theme.prompt(symbol);
        self
    }
    /// Replace the welcome banner.
    pub fn welcome(
        mut self,
        f: impl Fn(&Welcome, &Theme) -> String + Send + Sync + 'static,
    ) -> Self {
        self.welcome = Some(Arc::new(f));
        self
    }
    /// Render selected runtime events yourself.
    pub fn on_event(
        mut self,
        f: impl Fn(&HarnessEvent, &Theme) -> Option<String> + Send + Sync + 'static,
    ) -> Self {
        self.event_hook = Some(Arc::new(f));
        self
    }
    /// Where input history is kept (default: `<store>/.history`).
    pub fn history_file(mut self, p: impl Into<PathBuf>) -> Self {
        self.history_file = Some(p.into());
        self
    }

    /// Parse the generic flags from the command line and run.
    pub async fn run(self) -> std::process::ExitCode {
        use clap::{Args, FromArgMatches};
        let name = self.app.name().to_string();
        let cmd = ArkArgs::augment_args(
            clap::Command::new(clap::builder::Str::from(
                bin_name(&self.bin).leak() as &'static str
            ))
            .about(format!("{name} · an ARK agent")),
        );
        let matches = cmd.get_matches();
        match ArkArgs::from_arg_matches(&matches) {
            Ok(args) => self.run_with(args).await,
            Err(e) => e.exit(),
        }
    }

    /// Run with flags parsed by the caller (when the app has its own subcommands).
    pub async fn run_with(self, args: ArkArgs) -> std::process::ExitCode {
        let color = theme::want_color(std::io::stdout().is_terminal());
        match self.start(args, color).await {
            Ok(code) => code,
            Err(e) => {
                let t =
                    Theme::default().with_color(theme::want_color(std::io::stderr().is_terminal()));
                eprintln!("{} {e:#}", t.red("error:"));
                std::process::ExitCode::FAILURE
            }
        }
    }

    async fn start(self, args: ArkArgs, color: bool) -> anyhow::Result<std::process::ExitCode> {
        let bin = bin_name(&self.bin);
        let headless = args.print.is_some() || !std::io::stdin().is_terminal();
        let (chat, resumed, history_file) = match &args.connect {
            Some(url) => {
                let token = args
                    .token
                    .clone()
                    .or_else(|| std::env::var("ARK_TOKEN").ok())
                    .filter(|t| !t.trim().is_empty())
                    .ok_or_else(|| anyhow::anyhow!("--connect needs an access token: --token or ARK_TOKEN (made on that runtime, docs/REMOTE.md)"))?;
                // Nothing runs here but the terminal; a one-shot run may log to stderr.
                if headless {
                    ark_interact::logging::init(ark_interact::logging::LogTarget::Stderr);
                }
                let remote = remote::Remote::connect(url, &token, args.verbose).await?;
                (Chat::Remote(remote), false, self.history_file)
            }
            None => {
                let start = Start {
                    model: args.model.clone(),
                    project: args.project.clone(),
                    session: args.session.clone(),
                    resume: args.resume,
                };
                let (approver, approvals) = ChannelApprover::channel();
                let opened = self.app.open(&start, Arc::new(approver))?;
                let mut ix = opened.interaction;
                // Unless the application installed its own logger: a file in the store for
                // the shell (stderr would break the screen), stderr for a one-shot run.
                let target = if headless {
                    ark_interact::logging::LogTarget::Stderr
                } else {
                    ark_interact::logging::LogTarget::File(ark_interact::logging::file_in(
                        &ix.ark().settings().store_root,
                    ))
                };
                ark_interact::logging::init(target);
                ix.set_detail(args.verbose);
                ix.set_keys(
                    KEYS.iter()
                        .map(|(k, v)| (k.to_string(), v.to_string()))
                        .collect(),
                );
                let history_file = self
                    .history_file
                    .or_else(|| Some(ix.ark().settings().store_root.join(".history")));
                let local = Local {
                    ix,
                    approvals,
                    held: Vec::new(),
                };
                (Chat::Local(Box::new(local)), opened.resumed, history_file)
            }
        };
        let cfg = Arc::new(Config {
            bin,
            theme: self.theme.with_color(color),
            welcome: self.welcome,
            event_hook: self.event_hook,
            history_file,
        });
        let st = ShellState {
            cfg,
            chat,
            out: Out::stdout(),
        };

        // A prompt from -p, or piped stdin with no terminal: run once.
        let piped = if args.print.is_none() && !std::io::stdin().is_terminal() {
            let mut s = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut s)?;
            Some(s).filter(|s| !s.trim().is_empty())
        } else {
            None
        };
        if let Some(prompt) = args.print.or(piped) {
            let mode = if args.events {
                headless::Mode::Events
            } else if args.json {
                headless::Mode::Json
            } else {
                headless::Mode::Text
            };
            return Ok(headless::run(st, prompt, mode, args.yes).await);
        }
        Ok(shell::interactive(st, resumed).await)
    }
}

fn bin_name(explicit: &Option<String>) -> String {
    explicit.clone().unwrap_or_else(|| {
        std::env::args()
            .next()
            .and_then(|a| {
                std::path::Path::new(&a)
                    .file_name()
                    .map(|f| f.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| "ark".into())
    })
}
