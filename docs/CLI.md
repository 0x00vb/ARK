# ark-cli: the interactive shell

Every ARK app opens into a persistent agent session. You type what you want in plain words;
the agent works, shows what it is doing, asks before it changes anything, and remembers where
you left off. The same shell runs every ARK app, from the novel editor (`writer`) to a team of
agents (`agent-team`). Each app brings its own tools, commands, look and labels.

```text
$ writer

  Writer · powered by ARK
  a developmental editor that reads your manuscript before it opines
  model ollama qwen3:4b-instruct (single tier) · project the-locket · session 6cf6567a
  /help for commands · esc stops the agent · ctrl+d leaves

✎ how long is chapter 4, and where else does the locket appear?

● Planning
    Retrieve the text of chapter 4 to determine its length.
    Search the entire narrative for all instances of the locket.
    ...
● Reading the table of contents
  └ 4 chapters: chapter-01.md (46w), chapter-02.md (47w), chapter-03.md (43w), chapter-04.md …
● Tracing "locket" through the manuscript
  └ "locket": 2 mention(s) in chapter-01.md, chapter-02.md
● Reading chapter-04.md
  └ read chapter-04.md "Chapter 4: The Hinge" (95 words); full text via get_evidence(path="te…
● Recalling evidence ×5
(the answer streams here)

  3 tools · 25k tokens · 15.0s

✎ _
```

Captured from a real run with a local 4B model and the planner on (it is off by default since 0.7, so a current run starts at the first tool). The answer is trimmed, and the five repeated "Recalling evidence" lines are shown collapsed, as the current build prints them.

## Running an app

| Command | What it does |
|---|---|
| `app` | Open a new interactive session |
| `app --resume` (`-r`) | Continue the most recent session in this project |
| `app --session <id>` | Continue a specific session (id or unique prefix) |
| `app --project <name>` | Work in another project (created if missing) |
| `app --model <m>` | Use another model: `qwen3:4b`, `anthropic:claude-sonnet-5-5`, or a subscription: `claude:sonnet`, `codex:default` |
| `app --connect <url>` | Use the same app running elsewhere (see [On another machine](#on-another-machine)) |
| `app -v` | Start with the debug view on |
| `app -p "prompt"` | Run one prompt and exit (see [Headless](#headless-runs)) |
| `echo "prompt" \| app` | Same, prompt from stdin |

The model comes from `ARK_PROVIDER` / `ARK_MODEL` (and `ARK_MODEL_FAST` / `ARK_MODEL_STRONG`
for two tiers). `ARK_CTX_BUDGET` sets the per-request token budget.

## Keys

| Key | Action |
|---|---|
| enter | Send |
| alt+enter, ctrl+j, or a trailing `\` | New line |
| ↑ / ↓ | Input history (kept per store) |
| tab | Complete a `/command` |
| esc or ctrl+c (agent working) | Stop the turn. State so far is saved |
| typing, then enter (agent working) | Steer: the message reaches the agent at its next step |
| typing, then tab (agent working) | Queue: the message becomes the next prompt when the turn ends |
| ctrl+d or `/exit` | Leave. The session is saved |

## Slash commands

Every app has these. Apps add their own (`/device`, `/chapter`, ...), listed first in `/help`.

| Command | What it shows or does |
|---|---|
| `/help` | Commands and keys |
| `/new` | A fresh session. Working state and evidence stay |
| `/resume [id]` | Continue another session (the latest one if no id) |
| `/sessions` | Sessions in this project |
| `/rename <title>` | Give this session a title |
| `/project [name]` | List projects, or switch (created if new) |
| `/folder [path]` | Show the folder this project's tools work in, or change it |
| `/model [provider] [model]` | Show the model and the usable backends, or switch for the next turn |
| `/context` | What the model receives, by source, and what ARK kept out |
| `/usage` | Tokens, model calls and tool calls for this session |
| `/tools` | Every action, its risk tier, and whether it is preloaded |
| `/skills` | The skills of the project folder (`.agents/skills`) and the store (`skills`), and where to add more |
| `/plugin` | MCP servers: list; `add <name> [KEY=VALUE…] <command> [args…]` (or a pasted `mcpServers` snippet), `remove <name>`, `restart <name>`. A change applies from the next turn ([`EXTENSION.md`](EXTENSION.md#mcp-plugins)) |
| `/trace [n]` | The last turn step by step; `/trace 2` shows call 2's input and output |
| `/state` | The working state the agent carries between turns |
| `/status` | Model, project, session, context use, approvals, app sections |
| `/compact` | Tighten the carried context now |
| `/clear` | Clear the screen. The conversation is not affected |
| `/config` | Budget, steps, planner, critic, carry-over policy |
| `/debug [on\|off]` | Show arguments, durations and model calls inline |
| `/exit` | Leave |

With an orchestrator, `/agents`, `/agent` and `/task` are added ([`ORCHESTRATION.md`](ORCHESTRATION.md)).

## Approvals

Actions marked `Mutate` never run on their own. When the agent wants one, the shell asks:

```text
! The agent wants to run:

    $ cargo test --workspace
    Run a shell command in the workspace (build/run/test)

  › 1. Allow once
    2. Allow for this session (don't ask again for `code.exec:cargo`)
    3. Deny
```

Arrows and enter, or `1`/`y`, `2`/`a`, `3`/`n`. Esc denies. Ctrl+C denies and stops the turn.

"Allow for this session" is remembered by approval class. By default the class is the action
name. A profile can class calls more finely (`AgentProfile::approval_classifier`): classing shell
commands by program means allowing `cargo` does not allow `rm`. Grants live in memory and reset on `/new` and `/resume`.

The shell does not decide policy. The runtime's `ApprovalGate` does; the shell only shows the
question and returns the answer.

## Sessions and resume

A session is one conversation in a project. Every turn is stored in full, so you can leave and
come back:

```text
$ writer --resume
✓ Resumed save a short chapter five · 3 turns · 2h ago
  ✎ tighten it and save again
    Saved the tighter version.
  The agent continues from its working state; the transcript is not replayed.
```

Resuming does not replay the transcript into the model. The agent gets:

- the project's working state (goal, findings, hypotheses, open questions, next steps);
- its evidence cards, retrievable on demand;
- a bounded carry-over of the conversation (below).

## Context and compaction

The terminal transcript is not the model's context. They are kept apart on purpose.

Each turn the model receives the carry-over built by `Session::conversation_digest`:

- the last 3 exchanges, each clipped (300 characters of input, 500 of answer);
- one-line gists of up to 8 older turns;
- a count of anything older, whose conclusions are in the working state.

`/context` shows the breakdown of a real request (the output below predates the "Retrieved
context" row, which now separates evidence fetched with `get_evidence` from tool results):

```text
Context · last request (step 5)
  ──────────────────────────────────
  Instructions                   176
  Project                         70
  Capability index               123
  Working state                  105
  Conversation                    99
  Workspace                        0
  Task                            20
  Tool results                   235
  ──────────────────────────────────
  Used                           735
  Budget                        6.0k
  Tool schemas (extra)          1.1k

Kept out of the prompt
  transcript             4 turns stored (~89 tokens of text), 91 tokens carried into the next turn
  folded turns           1 as one-line gists, 0 merged into working state
  tool output            7 calls, ~456 raw → ~53 distilled into 4 evidence cards
```

Numbers use the same estimate as the budget manager (about 4 characters per token). The
provider's measured prompt size for each call is in `/trace`.

`/compact` runs the same rules harder, with no model call: keep only the latest exchange
verbatim, halve the gist list, and tidy the working state (drop duplicates, close hypotheses
that were rejected, keep the 6 most recent next steps and questions). Findings, open
hypotheses and rejections are never dropped. Nothing is deleted from the transcript or the
evidence store.

Measured on a 500-turn session (`crates/ark-sdk/tests/long_session.rs`, each turn one tool
call and a 1,500 character answer):

| Turn | Largest request (tokens) | Carried conversation | Full transcript |
|---:|---:|---:|---:|
| 1 | 457 | 153 | 389 |
| 10 | 1,144 | 712 | 3,890 |
| 50 | 1,236 | 767 | 19,450 |
| 100 | 1,236 | 767 | 38,901 |
| 250 | 1,239 | 770 | 97,401 |
| 500 | 1,239 | 770 | 194,901 |

The prompt stops growing once the gist window fills (about turn 12). At turn 500 the
transcript is 157 times larger than any request. The test fails if the request grows more
than 2% between turns 50 and 500.

## Headless runs

`-p` runs one turn with the same runtime, the same store and the same session tables. A
scripted run can be continued interactively later with `--resume`.

| Flags | Output |
|---|---|
| `-p "..."` | Answer on stdout (streamed), activity on stderr |
| `-p "..." --json` | One JSON object: answer, cited, steps, metrics, findings, session, context |
| `-p "..." --events` | Every runtime event as a JSON line, then a `{"type":"result",...}` line |
| `--yes` | Allow state-changing actions. Without it they are refused and the model is told, and any other question (an agent's) is denied |

Exit codes: 0 answered, 1 error, 130 stopped by Ctrl+C.

## On another machine

`--connect` opens the shell on the same app running elsewhere: its agents, schedules, store and
models (ADR 0024). The app serves itself with saved access tokens ([`REMOTE.md`](REMOTE.md));
nothing runs in the terminal's process but the terminal.

```text
$ export ARK_TOKEN=ark_…            # made on that runtime, e.g. `writer --new-token laptop`
$ writer --connect https://ark.example.com

  Writer · powered by ARK
  a developmental editor that reads your manuscript before it opines
  on ARK at https://ark.example.com
  model ollama qwen3:4b-instruct · project the-locket · session bfdfa551
  /help for commands · esc stops the agent · ctrl+d leaves

✓ Continuing How many words are in chapter 4? · 2 turns · just now
  › How many words are in chapter 4?
    Chapter 4 contains **95 words**. …
  The agent continues from its working state; the transcript is not replayed.
✎ _
```

Captured from a real run against `writer --serve 127.0.0.1:8431 --tokens` on the same machine
(address shortened; the answer is trimmed).

| What | How it works with `--connect` |
|---|---|
| The chat | The one open there: a window on the same runtime shows the same conversation. `/resume`, `/new`, `/project` and `/model` work as commands |
| A turn running when you connect | Shown so far, then followed to the end |
| Questions | Asked here; answered in a window, they close here |
| Esc, steering, queueing | As locally |
| `-p` | Same flags. `--json` has `ok`, `answer`, `cancelled`, `steps`, `session` and `project` |
| Leaving | The chat and its agents keep running there |
| Not with | `--resume`, `--session`, `--project`, `--model` (use the commands) |

The token comes from `--token` or `ARK_TOKEN`; prefer the variable, since a flag shows in the
process list.

## Logs

The interactive shell logs to `<store>/ark.log`, a headless run to stderr, at the level in
`ARK_LOG` (`warn` by default; `info` adds one line per turn with its tokens, calls and time).
Logs never contain the conversation. Settings for timeouts, retries and traces are in
[`OPERATIONS.md`](OPERATIONS.md).

## Building an app on the shell

An app is described once as an `ark_interact::App`: the agent, its commands, labels and
status. `ArkCli` opens it in the terminal; `ark_ui::ArkUi` opens the same `App` in a window
(see [`UI.md`](UI.md)). From `examples/writing-agent/src/main.rs`, trimmed:

```rust
let app = App::new("Writer")
    .tagline("a developmental editor that reads your manuscript before it opines")
    .ark(ark)
    .project(manuscript_project)
    .command(Arc::new(ManuscriptCmd))
    .command(Arc::new(ChapterCmd))
    .labels(labels())
    .status_section("Manuscript", |ark, project| { /* Vec<Block> */ });

ArkCli::new(app)
    .theme(Theme::default().accent(Accent::Magenta).prompt("✎ "))
    .run_with(cli.ark)
    .await
```

| `App` method | Purpose |
|---|---|
| `new(name)`, `.tagline`, `.help` | Branding and the text at the top of `/help` |
| `.ark(ark)`, or `.agent(profile)` + `.tool(..)` + `.store_root(..)` | The agent |
| `.project(f)` | Which project to open (default: one named after the app) |
| `.command(Arc<dyn Command>)` | A domain slash command |
| `.labels(ActivityLabels)`, `.labeler(Arc<dyn ToolLabeler>)` | How your tools read in activity and approvals |
| `.status_section(title, f)` | A block in `/status` |
| `.on_project_change(f)` | Called after a project switch |
| `.pricing(Pricing)` | Cost per million tokens for `/usage` (or `ARK_PRICE`) |

| `ArkCli` method | Purpose |
|---|---|
| `.bin(name)` | The executable name used in hints |
| `.theme(Theme)`, `.prompt(symbol)` | Accent colour, prompt, glyphs |
| `.welcome(f)` | Replace the welcome banner |
| `.on_event(f)` | Render selected runtime events yourself |
| `.history_file(path)` | Where input history lives (default `<store>/.history`) |
| `.run()` / `.run_with(ArkArgs)` | Parse flags and go, or take flags you parsed |

An app with its own subcommands flattens `ArkArgs` into its clap parser and calls
`run_with` when no subcommand is given. `examples/writing-agent` does this with its flags:
`writer` opens the shell, `writer --serve` serves the runtime.

### Domain commands

A command returns a structured `Reply`, never printed text. The shell renders the `Report`
as terminal lines; the window renders the same `Report` inline in the conversation. From
the writer:

```rust
#[async_trait::async_trait]
impl Command for ChapterCmd {
    fn name(&self) -> &str { "chapter" }
    fn summary(&self) -> &str { "Have the editor read and assess a chapter" }
    fn usage(&self) -> &str { "<n>" }
    fn group(&self) -> &str { "Writing" }
    async fn run(&self, ix: &mut Interaction, args: &str) -> anyhow::Result<Reply> {
        let ws = ix.engine().workspace_dir(&ix.project().id)?;
        let Some(file) = find_chapter(&ws, args) else {
            return Ok(Report::new().warning("Which chapter? e.g. /chapter 4 (see /manuscript)").into());
        };
        Ok(Reply::submit(format!("Read {file} and give me a short developmental assessment: ...")))
    }
}
```

`Interaction` gives the agent (`ark()`, `engine()`, `project()`, `session()`), `run_action`
for direct tool runs, and `new_session`, `resume`, `switch_project`. A `Report` is a title
and blocks (`heading`, `text`, `fields`, `table`, `code`, `meter`, `actions`) with a tone
each. `Reply::submit(text)` hands text to the agent; an `Effect` can also clear the view or
exit.

### Activity lines

Tool calls render as a verb and a subject. Without labels ARK derives one from the action
name and its main argument: `manuscript.read {"file":"ch4.md"}` reads as `Reading ch4.md`,
`continuity.search {"term":"locket"}` as `Searching "locket"`. Artifact ids are shown by name.
Label your own actions with `ActivityLabels` (exact name or whole namespace), or implement
`ToolLabeler` to also shape the result line and the approval preview. The labels live in
`ark-interact`, so the terminal and the window show the same words.

## How it fits together

```text
 ark-runtime (headless)      ark-interact (no drawing)        ark-cli (terminal)
 ┌──────────────────────┐    ┌──────────────────────────┐    ┌──────────────────────┐
 │ Harness::ask         │ ─► │ Interaction::run_turn    │ ─► │ driver: select! over │
 │ ApprovalGate ◄───────┼────┼─ ChannelApprover ◄───────┼────┼─ approval menu       │
 │ CancelToken, Steering│ ◄──┼─ TurnControl ◄───────────┼────┼─ esc, enter, tab     │
 │ Session (carry-over) │    │ commands → Report        │ ─► │ render: Report→lines │
 └──────────────────────┘    │ activity, trace, inspect │    │ view: Activity→lines │
                             └──────────────────────────┘    └──────────────────────┘
```

- The agent's turn is a future. The driver polls it together with the event channel, the
  approval channel, the keyboard and a 120 ms tick, so the screen stays live while the agent
  works and nothing blocks.
- The runtime never sees colours, spinners, prompts, keys or terminal size. It gets an event
  callback, a `CancelToken`, a `Steering` queue and an `ApprovalGate`.
- Commands, labels, traces and the `/context`, `/usage`, `/state` views are computed in
  `ark-interact`. `ark-cli` only turns them into lines; `ark-ui` turns the same values into
  a page.

Files in `crates/ark-cli/src`: `lib.rs` (builder, flags), `app.rs` (shell state),
`shell.rs` (input loop), `driver.rs` (turn loop), `view.rs` (activity rendering),
`render.rs` (`Report` to lines), `approval.rs` (menu), `markdown.rs` (streaming styling),
`headless.rs` (`-p`), `term.rs`, `theme.rs`.

## Testing an app without a model

`ARK_PROVIDER=mock ARK_MOCK_SCRIPT=script.json app` replays a scripted model:

```json
[
  { "tool_calls": [{ "name": "manuscript.read", "arguments": { "file": "chapter-04.md" } }] },
  { "content": "Chapter 4 is 95 words." }
]
```

`ARK_MOCK_DELAY_MS` paces the streamed words. This drives the real shell (approvals, cancel,
resume) in CI or through a pty.

## Limits

- Type-ahead during a turn is plain text (no cursor movement). Enter steers, tab queues.
- A steer that arrives after the agent's last step is not lost: it becomes the next prompt.
- The Markdown styling is light: headings, bold, inline code, fences, bullets.
- Token numbers in `/context` are estimates. Measured provider numbers are in `/trace`.
- "Always in this project" grants last until removed with `/grants remove <name>`; "for this
  session" grants end with the process.
- Shift+Enter inserts a newline only in terminals that report it; alt+enter and ctrl+j always work.
