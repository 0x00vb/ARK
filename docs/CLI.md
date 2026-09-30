# ark-cli: the interactive shell

Every ARK app opens into a persistent agent session. You type what you want in plain words;
the agent works, shows what it is doing, asks before it changes anything, and remembers where
you left off. The same shell runs a security workbench (`secres`) and a novel editor
(`writer`). Each app brings its own tools, commands, look and labels.

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

Captured from a real run with a local 4B model. The answer is trimmed, and the five repeated "Recalling evidence" lines are shown collapsed, as the current build prints them.

## Running an app

| Command | What it does |
|---|---|
| `app` | Open a new interactive session |
| `app --resume` (`-r`) | Continue the most recent session in this project |
| `app --session <id>` | Continue a specific session (id or unique prefix) |
| `app --project <name>` | Work in another project (created if missing) |
| `app --model <m>` | Use another model: `qwen3:4b` or `anthropic:claude-sonnet-5-5` |
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
| typing (agent working) | Queued for the next prompt. Press enter to send it when the turn ends |
| ctrl+d or `/exit` | Leave. The session is saved |

## Slash commands

Every app has these. Apps add their own (`/device`, `/chapter`, ...), listed first in `/help`.

| Command | What it shows or does |
|---|---|
| `/help` | Commands and keys |
| `/new` | A fresh session. Working state and evidence stay |
| `/resume [id]` | Continue another session (the latest one if no id) |
| `/sessions` | Sessions in this project |
| `/model [provider] [model]` | Show the model, or switch it for the next turn |
| `/context` | What the model receives, by source, and what ARK kept out |
| `/usage` | Tokens, model calls and tool calls for this session |
| `/tools` | Every action, its risk tier, and whether it is preloaded |
| `/trace [n]` | The last turn step by step; `/trace 2` shows call 2's input and output |
| `/state` | The working state the agent carries between turns |
| `/status` | Model, project, session, context use, approvals, app sections |
| `/compact` | Tighten the carried context now |
| `/clear` | Clear the screen. The conversation is not affected |
| `/config` | Budget, steps, planner, critic, carry-over policy |
| `/debug [on\|off]` | Show arguments, durations and model calls inline |
| `/exit` | Leave |

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
name. A profile can class calls more finely; SecRes classes shell commands by program, so
allowing `cargo` does not allow `rm`. Grants live in memory and reset on `/new` and `/resume`.

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

`/context` shows the breakdown of a real request:

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
| `--yes` | Allow state-changing actions. Without it they are refused and the model is told |

Exit codes: 0 answered, 1 error, 130 stopped by Ctrl+C.

## Building an app on the shell

```rust
use std::sync::Arc;
use ark_cli::{Accent, ActivityLabels, ArkCli, Theme};

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let ark = Ark::builder()
        .store_root(root)
        .tool(Arc::new(Manuscript))
        .profile(writer_profile())
        .build()
        .expect("model");

    ArkCli::builder()
        .name("Writer")
        .tagline("a developmental editor")
        .ark(ark)
        .project(|ark| ark.find_or_create_project("my-novel", ""))
        .theme(Theme::default().accent(Accent::Magenta).prompt("✎ "))
        .command(Arc::new(ChapterCommand))
        .labels(ActivityLabels::new().label("manuscript.read", |i| format!("Reading {}", i["file"])))
        .run()
        .await
}
```

| Builder method | Purpose |
|---|---|
| `.name`, `.tagline`, `.bin` | Branding and the executable name used in hints |
| `.ark(ark)` or `.agent(profile)` + `.tool(..)` + `.store_root(..)` | The agent |
| `.project(f)` | Which project to open (default: one named after the app) |
| `.theme(Theme)`, `.prompt(symbol)` | Accent colour, prompt, glyphs, turn footer |
| `.welcome(f)` | Replace the welcome banner |
| `.help(text)` | Text at the top of `/help` |
| `.command(Arc<dyn Command>)` | A domain slash command |
| `.labels(ActivityLabels)`, `.renderer(Arc<dyn ToolRenderer>)` | How your tools read on screen and in approvals |
| `.on_event(f)` | Render selected runtime events yourself |
| `.status_section(title, f)` | A block in `/status` |
| `.history_file(path)` | Where input history lives (default `<store>/.history`) |
| `.run()` / `.run_with(ArkArgs)` | Parse flags and go, or take flags you parsed |

An app with its own subcommands flattens `ArkArgs` into its clap parser and calls
`run_with` when no subcommand is given. SecRes does this: `secres` opens the shell,
`secres health` stays a plain command.

### Domain commands

```rust
struct Chapter;

#[async_trait::async_trait]
impl ark_cli::Command for Chapter {
    fn name(&self) -> &str { "chapter" }
    fn summary(&self) -> &str { "Have the editor assess a chapter" }
    fn usage(&self) -> &str { "<n>" }
    fn group(&self) -> &str { "Writing" }
    async fn run(&self, ctx: &mut CommandContext<'_>, args: &str) -> anyhow::Result<CommandOutcome> {
        Ok(CommandOutcome::Submit(format!("Read chapter {args} and assess it.")))
    }
}
```

`CommandContext` gives the agent (`ark()`, `engine()`, `project()`, `session()`), output
(`line`, `heading`, `row`), `run_action` for direct tool runs, and `new_session`, `resume`,
`switch_project`. A command returns `Continue`, `Submit(text)` to hand text to the agent, or
`Exit`.

### Activity lines

Tool calls render as a verb and a subject. Without labels ARK derives one from the action
name and its main argument: `manuscript.read {"file":"ch4.md"}` reads as `Reading ch4.md`,
`continuity.search {"term":"locket"}` as `Searching "locket"`. Artifact ids are shown by name.
Label your own actions with `ActivityLabels` (exact name or whole namespace), or implement
`ToolRenderer` to also shape the result line and the approval preview.

## How it fits together

```text
 ark-runtime  (headless)                     ark-cli  (terminal)
 ┌───────────────────────────┐   events      ┌───────────────────────────┐
 │ Harness::ask              │ ────────────► │ driver: select! over      │
 │  budget, distill, dedup   │               │   events, approvals,      │
 │  ApprovalGate ◄───────────┼── decisions ──│   keys, spinner tick      │
 │  CancelToken ◄────────────┼── esc/ctrl+c ─│ view: events → activity   │
 │ Session (carry-over)      │               │ shell: reedline, /cmds    │
 └───────────────────────────┘               └───────────────────────────┘
```

- The agent's turn is a future. The driver polls it together with the event channel, the
  approval channel, the keyboard and a 120 ms tick, so the screen stays live while the agent
  works and nothing blocks.
- The runtime never sees colours, spinners, prompts, keys or terminal size. It gets an event
  callback, a `CancelToken` and an `ApprovalGate`. A GUI or a server supplies the same three.
- `HarnessEvent` is owned and serializable. The terminal view, `--events` JSON and the Tauri
  desktop app all consume the same stream.

Files in `crates/ark-cli/src`: `lib.rs` (builder, flags), `shell.rs` (input loop),
`driver.rs` (turn loop), `view.rs` (event rendering), `approval.rs` (menu),
`commands.rs` (slash commands), `activity.rs` (labels), `markdown.rs` (streaming styling),
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

- Type-ahead during a turn is plain text (no cursor movement). It becomes the next prompt.
- The Markdown styling is light: headings, bold, inline code, fences, bullets.
- Token numbers in `/context` are estimates. Measured provider numbers are in `/trace`.
- Approval grants are not persisted across processes.
- Shift+Enter inserts a newline only in terminals that report it; alt+enter and ctrl+j always work.
