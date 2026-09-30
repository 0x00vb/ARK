//! # ark-cli
//!
//! The interactive terminal shell every ARK application enters by default. Launching the app
//! drops you into a persistent agent session; `-p` runs one prompt for scripts and CI. Both
//! paths drive the same runtime.
//!
//! ```ignore
//! use ark_cli::{ArkCli, Accent, Theme};
//!
//! #[tokio::main]
//! async fn main() -> std::process::ExitCode {
//!     ArkCli::builder()
//!         .name("Writer")
//!         .tagline("a developmental editor for your novel")
//!         .ark(build_ark()?)                       // tools + AgentProfile + model
//!         .project(|ark| ark.find_or_create_project("my-novel", ""))
//!         .theme(Theme::default().accent(Accent::Magenta).prompt("✎ "))
//!         .command(std::sync::Arc::new(ChapterCommand))
//!         .run()
//!         .await
//! }
//! ```
//!
//! The runtime never learns about the terminal. The shell hands it a cancel token, an
//! approval gate and an event callback; everything visual happens here.

mod activity;
mod app;
mod approval;
mod commands;
mod driver;
mod headless;
mod markdown;
mod shell;
mod term;
mod theme;
mod view;

use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Arc;

use ark_sdk::prelude::*;

pub use activity::{default_label, ActivityLabels, ToolRenderer};
pub use app::{EventHook, TurnRecord, Welcome};
pub use commands::{Command, CommandContext, CommandOutcome};
pub use markdown::MdStream;
pub use term::Out;
pub use theme::{Accent, Glyphs, Theme};

use app::{Config, ShellState};
use approval::TerminalApprover;

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
}

type ProjectFn = Box<dyn Fn(&Ark) -> Result<Project, ArkError> + Send + Sync>;

/// Entry point. See the crate docs.
pub struct ArkCli;

impl ArkCli {
    pub fn builder() -> ArkCliBuilder {
        ArkCliBuilder::default()
    }
}

#[derive(Default)]
pub struct ArkCliBuilder {
    name: Option<String>,
    bin: Option<String>,
    tagline: Option<String>,
    theme: Theme,
    welcome: Option<app::WelcomeFn>,
    help_intro: Option<String>,
    ark: Option<Ark>,
    ark_builder: Option<ArkBuilder>,
    project: Option<ProjectFn>,
    commands: Vec<Arc<dyn Command>>,
    renderers: Vec<Arc<dyn ToolRenderer>>,
    event_hook: Option<EventHook>,
    status: Vec<(String, app::StatusFn)>,
    history_file: Option<PathBuf>,
}

impl ArkCliBuilder {
    /// The application's display name.
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }
    /// The executable name used in hints (`<bin> --resume`). Defaults to the running binary.
    pub fn bin(mut self, bin: impl Into<String>) -> Self {
        self.bin = Some(bin.into());
        self
    }
    /// One line under the name in the welcome banner.
    pub fn tagline(mut self, t: impl Into<String>) -> Self {
        self.tagline = Some(t.into());
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
    /// Text shown at the top of `/help`.
    pub fn help(mut self, intro: impl Into<String>) -> Self {
        self.help_intro = Some(intro.into());
        self
    }
    /// The fully wired agent (tools, profile, model, store).
    pub fn ark(mut self, ark: Ark) -> Self {
        self.ark = Some(ark);
        self
    }
    /// Build the agent here instead: the builder is finished at `run` time, after `--model`.
    pub fn ark_builder(mut self, b: ArkBuilder) -> Self {
        self.ark_builder = Some(b);
        self
    }
    /// Shorthand: set the agent profile on the internal builder.
    pub fn agent(mut self, profile: AgentProfile) -> Self {
        self.ark_builder = Some(self.ark_builder.take().unwrap_or_default().profile(profile));
        self
    }
    /// Shorthand: register a tool on the internal builder.
    pub fn tool(mut self, adapter: Arc<dyn Adapter>) -> Self {
        self.ark_builder = Some(self.ark_builder.take().unwrap_or_default().tool(adapter));
        self
    }
    /// Shorthand: where the store lives.
    pub fn store_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.ark_builder = Some(self.ark_builder.take().unwrap_or_default().store_root(root));
        self
    }
    /// Which project the shell works in (default: one named after the app).
    pub fn project(
        mut self,
        f: impl Fn(&Ark) -> Result<Project, ArkError> + Send + Sync + 'static,
    ) -> Self {
        self.project = Some(Box::new(f));
        self
    }
    /// Add a domain slash command.
    pub fn command(mut self, c: Arc<dyn Command>) -> Self {
        self.commands.push(c);
        self
    }
    /// Custom activity lines for your tools.
    pub fn renderer(mut self, r: Arc<dyn ToolRenderer>) -> Self {
        self.renderers.push(r);
        self
    }
    /// Shorthand for `renderer(Arc::new(labels))`.
    pub fn labels(self, labels: ActivityLabels) -> Self {
        self.renderer(Arc::new(labels))
    }
    /// Render selected runtime events yourself.
    pub fn on_event(
        mut self,
        f: impl Fn(&HarnessEvent, &Theme) -> Option<String> + Send + Sync + 'static,
    ) -> Self {
        self.event_hook = Some(Arc::new(f));
        self
    }
    /// Add a titled block to `/status`.
    pub fn status_section(
        mut self,
        title: impl Into<String>,
        f: impl Fn(&Ark, &Project) -> Option<String> + Send + Sync + 'static,
    ) -> Self {
        self.status.push((title.into(), Arc::new(f)));
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
        let name = self.name.clone().unwrap_or_else(|| "ARK".into());
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
        let theme_color = theme::want_color(std::io::stdout().is_terminal());
        match self.start(args, theme_color).await {
            Ok(code) => code,
            Err(e) => {
                let t =
                    Theme::default().with_color(theme::want_color(std::io::stderr().is_terminal()));
                eprintln!("{} {e:#}", t.red("error:"));
                std::process::ExitCode::FAILURE
            }
        }
    }

    async fn start(mut self, args: ArkArgs, color: bool) -> anyhow::Result<std::process::ExitCode> {
        let name = self.name.take().unwrap_or_else(|| "ARK".into());
        let bin = bin_name(&self.bin);
        let mut ark = match (self.ark.take(), self.ark_builder.take()) {
            (Some(a), _) => a,
            (None, Some(b)) => b.build()?,
            (None, None) => Ark::builder().build()?,
        };
        if let Some(spec) = &args.model {
            let (provider, model) = match spec.split_once(':') {
                Some((p, m)) if ["ollama", "openai", "anthropic"].contains(&p) => {
                    (p.to_string(), m.to_string())
                }
                _ => (
                    std::env::var("ARK_PROVIDER").unwrap_or_else(|_| "ollama".into()),
                    spec.clone(),
                ),
            };
            ark.set_router(ModelRouter::single(ark_model::build(
                &provider,
                Some(model),
            )?));
        }

        let project = match &args.project {
            Some(p) => ark.find_or_create_project(p, "")?,
            None => match &self.project {
                Some(f) => f(&ark)?,
                None => ark.find_or_create_project(&name.to_lowercase(), "")?,
            },
        };

        let mut resumed = false;
        let session = if let Some(id) = &args.session {
            resumed = true;
            ark.open_session(id)?
                .ok_or_else(|| anyhow::anyhow!("no session matches `{id}`"))?
        } else if args.resume {
            match ark.latest_session(&project.id)? {
                Some(s) => {
                    resumed = true;
                    s
                }
                None => ark.start_session(&project.id)?,
            }
        } else {
            ark.start_session(&project.id)?
        };
        // A resumed session may live in another project.
        let project = if session.project() != &project.id {
            ark.engine()
                .store
                .get_project(session.project())?
                .unwrap_or(project)
        } else {
            project
        };

        let (approver, approvals) = TerminalApprover::channel();
        let gate = Arc::new(ApprovalGate::new(Arc::new(approver)));
        let mut all_commands = self.commands;
        all_commands.extend(commands::builtins());
        let history_file = self
            .history_file
            .or_else(|| Some(ark.settings().store_root.join(".history")));
        let cfg = Arc::new(Config {
            name,
            bin,
            tagline: self.tagline,
            theme: self.theme.with_color(color),
            welcome: self.welcome,
            help_intro: self.help_intro,
            activity: activity::Activity::new(self.renderers),
            event_hook: self.event_hook,
            status: self.status,
            commands: all_commands,
            history_file,
        });
        let st = ShellState {
            cfg,
            ark,
            project,
            session,
            gate,
            approvals,
            out: Out::stdout(),
            last_turn: None,
            last_context: None,
            verbose: args.verbose,
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
