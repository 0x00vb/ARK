# Changelog

What changed for people building on ARK, per release. Not a commit log.

- Sections: `Added`, `Changed`, `Fixed`, `Breaking`. A `Breaking` entry says what an app must
  change.
- New entries go under `[Unreleased]`; a release renames it (see `docs/RELEASING.md`).
- One line per change where possible. Link to a doc instead of explaining it here.

## [Unreleased]

### Added
- `/agent` and `/task` in both front-ends: add, pause, resume, end and message agents, hand
  out, cancel and retry tasks. `ark_interact::orchestration::{Control, ControlOutcome,
  control, parse_agent, parse_task}`; the window's methods run the same `control`.
- `ark-bench`: the naive baseline loop (`--harness naive`, `ark_bench::naive`), orchestrated
  workloads (`Workload::team`, `suite::TeamSpec`, the `team` workload), fault injection
  (`--faults`, `--seed`, `ark_bench::faults`), record and replay (`--record`, `--replay`,
  `ark_bench::cassette`), per-run traces (`--traces`). `RunRecord.faults_injected` and
  `.replay_diverged`. See `docs/BENCH.md`.
- Action preloading, off by default: `Harness::preload(n)` (`ARK_PRELOAD_ACTIONS=n`) loads up
  to `n` actions whose name or summary matches the task's words before the first step
  (`ark_runtime::catalog::matching_actions`), saving the `load_action` round trips. On by
  default only if live runs pay for it (ROADMAP, P2).
- `ark-bench` reports record runtime settings read from the environment (`env.*`).
- Window: projects are folders. The sidebar lists projects with their chats beneath, folds
  the ones not current, and renames a project or changes its folder from its menu.
  `Interaction::{open_project, add_project, rename_project, set_project_folder,
  project_sessions}`; page methods `open_project`, `add_project`, `rename_project`,
  `project_folder`, `sessions {project}`.
- Window: a top bar with tabs for app views and buttons for app panels marked `nav`, and a
  Settings page whose sections apps register with `ark.setting` (⌘,).
- The chat creates agents and hands them tasks: in an orchestrating app each turn gets
  `Orchestrator::chat_tools()` (`agents.create`, `agents.delegate`, `agents.wait`), so a
  person asks in words instead of filling a form. `TurnOptions::tools` and `TurnTools` add
  tools to one turn, offered directly; `Ark::context_preview_with` previews them. See
  `docs/ORCHESTRATION.md`, "From the chat", and ADR 0007.
- Window: `ark.compose(text)` puts text in the composer, unsent.
- Window: Terminal, Browser and Files in every app, opened from a + menu in the top bar
  (⌃`, ⌘T, ⌘P) or by the agent's `present` (`terminal`, `browser`, `file`). The terminal is
  the project's shell in its folder; Files browses, edits and saves files inside the folder.
  Page methods `term_open`, `term_write`, `term_resize`, `term_close`, `files`, `file_read`,
  `file_write`; event `term`. A `present` the window cannot show is a toast instead of nothing.
- Whole small results, off by default: `DistillerSet::whole_below(tokens)`
  (`ARK_WHOLE_RESULT_TOKENS`) passes a result no distiller recognizes whole when it is that
  small, instead of a summary the model may have to open with `get_evidence`.

### Changed
- Switching or opening a project returns to its most recent session instead of starting a
  new one (`Interaction::set_project`).
- Window: `nav` on `ark.view` and `ark.panel` puts the view in a top-bar tab and the panel
  in the top bar's + menu, not in the sidebar. "New session" is "New chat".
- **Action preloading is on by default** (2 actions; `ARK_PRELOAD_ACTIONS=0` turns it off).
  Measured live on qwen3:4b and Claude Haiku (`bench/RESULTS.md`, P1).
- Preloading drops a match from another adapter that scores below the best match minus 1
  (one coincidental word no longer brings in a distractor), and keeps an adapter's weaker
  matches next to its strong one (find, then list).
- **The first message names the loaded actions** under `== READY TO CALL ==`, one line each
  with its summary, so a model calls them instead of loading others from the index.
  `ARK_READY_LIST=0` turns it off. Counted under the capability index in `ContextReport`.
- **The generic summary shows a small array whole** (up to 400 characters of JSON) instead
  of its count, and says how to read the rows of an array it counts (`get_evidence` with
  `match`). `ARK_INLINE_ROWS=0` restores the count only. Results with a distiller are
  unchanged. Measured live on qwen3:4b (`bench/RESULTS.md`, P2).
- **The planner is off by default** (`Harness`, `ArkBuilder`, `Orchestrator` agents,
  `BenchSettings`). Its steps cost calls on small tasks and helped none: on qwen3:4b, 42/42
  without it against 41/42, 0.0181 against 0.0189 per success (`bench/RESULTS.md`, P2
  overhead; ADR 0008). `.planner(true)` or `AgentSpec::planner(true)` turns it back on;
  `ark-bench --planner` measures it.
- `ark-bench`'s simulated model reads raw JSON results as it reads ARK's cards, and finds
  the task in a plain transcript. ARK's simulated numbers do not move.
- `bench/baseline.json` has the `team` workload (14 workloads); `bench/baseline-naive.json`
  is the naive loop on the same suite.

### Breaking
- `ark_bench::BenchSettings` has `harness`, `faults`, `cassette` and `traces`: struct
  literals add them, or end with `..Default::default()`.

### Fixed
- A task waiting on a paused agent ends as soon as it is cancelled (it noticed within 250 ms).

## [0.6.0] - 2026-10-03

Several agents as compositions of a few primitives, run on subscriptions first.
Guide: `docs/ORCHESTRATION.md`. Example: `examples/agent-team`.

### Added
- **`ark-orchestrate`**: agents (`AgentSpec`: identity, instructions, model, allowed tools,
  delegation permissions, parent, budget), tasks (`TaskSpec`/`TaskRecord`: objective, state,
  owner, dependencies, artifacts, budget, attempts), delegation (`agents.delegate`,
  `agents.wait`), messages (`agents.message`), lifecycle (`spawn`, `run`, `suspend`,
  `resume`, `cancel`, `terminate`, `retry`), a scheduler that respects each account's
  capacity, budgets (steps, tokens, model calls, time, delegations), crash recovery, and an
  observable `Graph` with a stored event stream (`subscribe`, `events`, `task_trace`,
  `Graph::to_mermaid`). Each agent works in its own child project; a delegated agent gets a
  brief, never the delegator's conversation.
- **Managed vendor sessions** (`ark_model::session`): `SubscriptionRuntime` gives each lane
  (agent) its own `ManagedSession`, a `Provider` that resumes the vendor session with only
  what is new, monitors its context, rotates before `max_context_tokens`, rehydrates from
  ARK's budgeted request, recovers after a crash (`SessionLedger`), and releases idle
  sessions. `CliModel` implements `SessionBackend` (Claude `--session-id`/`--resume`, Codex
  `exec`/`exec resume`). Settings: `ARK_SESSION_REUSE`, `ARK_SESSION_MAX_CONTEXT`,
  `ARK_SESSION_MAX_TURNS`, `ARK_SESSION_IDLE_S`.
- **Capacity** (`ark_model::capacity`): `Capacity`, `Limiter` and `Limited` queue calls per
  account (default 2 at a time for `claude` and `codex`, 4 otherwise;
  `ARK_<ACCOUNT>_CONCURRENCY`) and cool down after a rate or usage limit.
- **Rule sets**: `ark_core::{RuleSet, Rule, RuleBinding, RuleScope, RuleRef}`, stored and
  versioned with `ark_runtime::RuleBook` (`Ark::rule_book`), bound to a project, agent or task,
  selected per task by `ark_runtime::rules::select`, looked up with the `rules` tool
  (`RulesTool`), and traced (`HarnessEvent::Rules`, `TaskRecord.rules`). Project bindings
  apply to single-agent turns too.
- **Diagrams**: `ark_tools::diagram` (`Diagram`, `DiagramKind`, `validate`; not in `ark-core`, which names no file format) and the opt-in
  `ark_tools::diagram::Diagrams` tool (`diagram.create`, `diagram.read`) store validated
  Mermaid diagrams as `diagram` artifacts, with versions.
- `Registry::filtered` and `ark_tools::action_matches`: a per-agent view of the tools.
  `Engine::scoped`: an engine over the same store with another registry.
- `Store` typed records (`put_record`, `get_record`, `list_records`, `delete_record`) and
  event streams (`append_event`, `list_events`, `last_event`).
- `Project.parent`; front-ends list only top-level projects.
- `ark_model::Message` and `ToolDef` derive `Serialize` and `Deserialize`.
- `ark_interact::orchestration` and `App::orchestrator`: `/agents` and `/agents <task>` in
  both front-ends. Workspace protocol methods `graph` and `task_trace`, events
  `{kind: "orch"}`.
- `examples/agent-team`: a coordinator and two readers on different subscriptions.
- Orchestration from a window: `App::orchestrate(f)` builds one orchestrator per project, the
  first time the interaction works there, with an approval gate that reaches the front-end.
  Workspace protocol methods `agents`, `spawn_agent`, `submit_task`, `cancel_task`,
  `retry_task`, `suspend_agent`, `resume_agent`, `terminate_agent`, `message_agent`, `task`
  (record and timeline), `orch_events` (stored events) and `tools`. They answer while a
  conversation turn runs. `orch` events carry the timeline `line` and, for an agent's runtime
  event, the `activity` the conversation would show. See `docs/UI.md`.
- `ApprovalRequest.agent` and `.task`: an orchestrated agent's question says who asks. The
  window shows it in a tray outside the conversation, and a turn's end no longer closes it.
- `ark.view({ id, title, icon, nav, render })`: an app view in the main column, in place of
  the conversation, listed in the sidebar (`ark.openView`, `ark.showConversation`).
- Labels for ARK's own tools: "Delegating to reader-a: …", "Messaging lead", "Drawing …".
- `Ark::session_router`, `Ark::subscriptions`, `ArkBuilder::subscriptions`: the runtime
  behind single-agent managed sessions (see Changed); its state is in the store
  (`ark_runtime::StoreLedger`). An orchestrator shares the `Ark`'s runtime and capacity.
- When ARK lets a vendor session go it deletes the vendor's file for that id
  (`SessionBackend::discard`; `ARK_SESSION_KEEP_FILES=1` keeps them).
- `SubscriptionRuntime::stored` and `ModelResolver::stored_session`: the graph shows an
  agent's stored session before it runs again after a restart.
- `examples/writing-agent`: `/review <n>`, an editor and two readers per manuscript project.
- Named profiles: `OrchestratorBuilder::profile(name, summary, profile)`,
  `Orchestrator::profiles`, `AgentSpec::profile_name` (stored, unlike `AgentSpec::profile`,
  so it survives a restart and a front-end can choose it), `AgentNode.profile`. Workspace
  protocol method `profiles`; `/agents` shows an agent's profile next to its model.
- The project folder in ARK: `Engine::set_workspace`, `Interaction::workspace` and
  `::set_workspace`, `/folder [path]` in both front-ends, workspace protocol method
  `workspace` (`{path}` sets it), `ProjectSummary.workspace`.
- Connection events reach app scripts in a browser: `ark:reconnecting`, `ark:disconnected`
  (with `retry()`), `ark:connected`.
- `ark.pickFolder({ title })`: the window's own folder picker (`undefined` in a browser,
  where the page cannot see the runtime's disk).
- `ark_interact::approval_title`: the approval headline both front-ends show. The terminal
  now names the agent and task that ask ("reader-a (task t-1a2b3c4d) wants to run:"); the
  window's `approval` events carry it as `title`.

### Changed
- **Single-agent turns on a subscription keep a vendor session.** With a `claude:` or
  `codex:` model, `Ark::ask_session` resumes one managed vendor session per conversation and
  sends only what is new (median uncached input 59.3k to 37.8k in `bench/RESULTS.md`).
  `ARK_SESSION_REUSE=0` restores stateless calls.
- A CLI rate or usage limit is `ProviderError::Overloaded` (was `Api`), so the harness
  retries it after the account's cool-down. Apps that matched `Api` for it match `Overloaded`.
- Headless `-p "/command"` runs the slash command and prints its report, instead of sending
  the text to the model.
- `ark-interact` depends on `ark-orchestrate` and re-exports it (`ark_interact::ark_orchestrate`).
- An orchestrated agent's project has no folder of its own: it works in its top-level
  project's folder, read when it runs, so changing the folder moves existing agents too.
- Claude Code is told its only tool is `StructuredOutput`. With the earlier wording Sonnet
  first tried to call a host tool by name and failed: one delegation took 4 internal turns
  and 3,049 input tokens, now 2 turns and 1,384 (measured with `claude -p`, 2 runs each).
- `ContextReport.rules` and a `Rules` row in `/context`.
- Results of `rules.*` and `diagram.*` reach the model in full instead of a generic summary.
- Claude Code gets ARK's tools as bullets ("use them only inside StructuredOutput"), not
  Markdown headings. With headings Haiku gave up a one-tool task 2 of 2 times; with bullets
  it made the right call in the minimum 2 turns. Sonnet is unchanged. Probes: `bench/protocol/`.
- A delegated result clipped in the delegator's evidence card says how to read the rest.
- Claude Code: a reply that took more than 2 internal turns and asked for no tool, while
  tools were offered, means the model gave up after refused native calls; ARK asks once more
  with a one-line correction, in the same vendor session. Seen once in 9 live runs with a Haiku reader.

### Fixed
- Agents created before the project folder was chosen kept working in the old folder.
- The window opens artifacts an orchestrated agent produced (they live in its child project).
- `ark.h` sets CSS custom properties given in `style` (`{ "--hue": 200 }`).

### Breaking
- `Engine.store` is an `Arc<Store>`. Method calls are unchanged; code that moved the store
  out of an engine takes a clone of the `Arc`.
- `HarnessEvent::Rules` is a new variant: add an arm where you match the enum exhaustively.
- `ContextReport` has a `rules` field and `Project` a `parent` field: struct literals add
  them (or use `..Default::default()` / `Project::new`).
- `ark_interact::trace::EntryKind::Rules` is a new variant.
- `ApprovalRequest` has `agent` and `task` fields: struct literals add `agent: None, task: None`.
- `ProjectSummary` has a `workspace` field: struct literals add `workspace: None`.
- `ark_sdk::prelude` exports `Rule`, `RuleSet`, `RuleBinding`, `RuleScope`, `RuleBook`,
  `RuleSelection`, `Diagram` and `DiagramKind`; a glob import of another module with the same
  names becomes ambiguous.

## [0.5.0] - 2026-10-01

### Added
- Subscription models through the vendor's official CLI: providers `claude` (Claude Code,
  Claude subscription) and `codex` (Codex, ChatGPT plan), e.g. `--model claude:sonnet`. ARK
  runs the tools; the CLI's own tools are off. See `docs/OPERATIONS.md#subscriptions`.
- `ark_model::catalog()`: the backends usable on this machine (signed-in subscriptions,
  Ollama models, APIs with a key) and the command that fixes one that is not ready.
- Workspace: the model menu lists those backends and switches with one click. New protocol
  method `models`. `/model` with no argument lists them too.

### Changed
- The model chip and `Summary.model` name the model only (`ModelRouter::label`), without
  "(single tier)". `/model` still shows the routing.
- Documentation reorganized for maintainers: `AGENTS.md` (rules and change lifecycle; `CLAUDE.md`
  imports it), `ROADMAP.md` at the root (state, limits, next), `docs/RELEASING.md` (release,
  app upgrade, rollback; replaces `docs/VERSIONING.md`), diagrams in `docs/ARCHITECTURE.md`,
  provider and development environment variables in `docs/OPERATIONS.md`.
  `docs/EXTRACTION_MAP.md` is folded into `docs/ARCHITECTURE.md` (Origin).
- Comments that described the extraction history now describe the code. No library change.

## [0.4.1] - 2026-10-01

### Fixed
- `scripts/ci.sh` ran the benchmark from `./target/release` even when `CARGO_TARGET_DIR`
  pointed elsewhere, so `scripts/verify-release.sh` (which honors `CARGO_TARGET_DIR`) failed
  at its last step. Found by verifying v0.4.0 from a fresh clone. No library change.

## [0.4.0] - 2026-10-01

Production hardening, measurement, and a cheaper harness. No new front-end surface.
Before/after numbers are in `bench/RESULTS.md`; the benchmark harness is in `docs/BENCH.md`
and running ARK in production in `docs/OPERATIONS.md`.

### Added
- **`ark-bench`**: a benchmark and eval harness for any ARK application. Workloads, a metered
  provider that records every request (size by part, measured usage, latency, first token,
  prefix shared with recent requests), a simulated model whose policy reacts to what each
  request contains, JSON reports and comparisons with regression gating, and a generic suite
  of 13 workloads. `bench/at-commit.sh` builds the current bench against an earlier commit.
- Provider resilience: `ProviderError::{Status, Overloaded, Malformed}` and
  `is_transient()`; `HttpConfig` (connect, idle and request timeouts, output cap);
  `RetryPolicy` (`ARK_MODEL_RETRIES`, default 2) used by the harness; `HarnessEvent::Retry`;
  `ModelCall.retries`, `Metrics.provider_retries`.
- `Reuse { Auto, Stable, Volatile }` on `ActionDescriptor` (`.reusable()`, `.volatile()`).
- `get_evidence` takes `match` and `offset`; `ark_runtime::evidence`.
- Context reports for every model call: `ContextReport.role`, `model_output`,
  `action_schemas`; the planner, critic and forced answer emit one.
- `Session::begin_turn` / `fail_turn`, `Turn.{incomplete, error, pid}`,
  `Engine::recover_interrupted`, `Invocation.pid`, `session::INTERRUPTED`.
- `ark_interact::logging` (`ARK_LOG`, file or stderr), `TraceMode` (`ARK_TRACE`),
  `TRACES_KEPT`; `Store::{project_stats, session_text_chars, prune_turn_traces,
  list_running_invocations}`.
- `TurnOptions::present`, `Harness::presentation`, `Interaction::set_presentation`,
  `Ark::context_preview_for`.
- `Usage::cache_write_tokens`, `Usage::uncached()`, `Metrics.cache_write_tokens`.
- Events: `HarnessEvent::Stall`, `ModelCall.first_token_ms`, `ToolResult.raw_tokens`.
- `scripts/ci.sh`, `scripts/check-version.sh`, `scripts/verify-release.sh`,
  `.github/workflows/ci.yml`.

### Breaking
- `ActionDescriptor::reusable: bool` is replaced by `reuse: Reuse`. `.reusable()` still
  marks an action `Stable`.
- `Selection` keeps `text` and `line`; `address`, `symbol` and `function` become named
  `marks` (flattened in JSON, so `{"address": "0x1000"}` still parses). Use
  `Selection::mark(name)`.
- The runtime tools offered by default are `load_action`, `get_evidence`, `search_evidence`
  and `record_state`. `list_actions` and `record_note` are no longer offered (still handled);
  `present` is offered only when the front-end can present.
- Request layout: the working state is the last message of each request instead of part of
  the system prompt; the system prompt is fixed for a turn and ordered instructions, index,
  project; loaded tools keep their load order; artifacts are listed as `art:<12 hex>`, at
  most 12. Default prompt text changed accordingly (a MINOR behavior change by our policy;
  in 0.x it ships with the breaking release).
- Anthropic requests carry `cache_control` breakpoints and merge consecutive messages of
  one role; the default output cap is 8192 (was 2048). Ollama sends `num_predict` (4096).
- New fields on `HarnessEvent::ModelCall` and `ToolResult`, new variants `Retry` and `Stall`:
  exhaustive matches need the new arms.
- The default activity labeler no longer treats `process`, `function` or `symbol` arguments
  as the subject.
- A tool call whose arguments are not valid JSON is `ProviderError::Malformed`, not `{}`.

### Fixed
- The stall guard counted every retrieval as progress, so a model repeating one search ran
  to the step limit.
- A read after a write in the same turn could be served the stale first result.
- A short artifact id and the full id were different calls to the dedup check.
- `get_evidence` could not reach rows past the first 40 of a large output.
- With no output cap and Ollama's context shifting, one degenerate reply ran for over 20
  minutes; there was no request or idle timeout on any provider.
- The common distiller summarized any `*.read` / `*.write` / `*.list` action as a file
  operation and dropped its fields (`notes.read` became "read  (0 lines)").
- The working-state digest showed the oldest six items of each section, not the newest.
- After every turn the workspace loaded the whole transcript and every stored tool output to
  count them; memory and turn time grew with each turn.
- A turn killed mid-run, or ending in an error, vanished from the transcript; a tool call
  killed mid-run left no record.
- The browser event stream reconnected every few seconds forever, and a reconnect during a
  turn could lose or repeat events.
- The HTTP server spun when `accept` kept failing.
- Warnings (for example a plugin that failed to load) went nowhere: no logger was installed.
- A reply that writes a tool call as text (`record_state({...})`) is no longer taken as the
  final answer: the model is told once that nothing ran and asked again. With the working
  state as the last message, qwen3:4b did this in 4 of 39 live runs.

## [0.3.0] - 2026-09-30

An interaction layer both front-ends share, and a graphical workspace on it.

### Added
- **`ark-interact`**: the layer between the runtime and any front-end. `App` (an app described
  once), `Interaction` (project, session, approvals, turns, views), `Command` / `Reply` /
  `Effect`, structured `Report`s (`Block`, `Table`, `Tone`, ...), `Labels` / `ToolLabeler` /
  `ActivityLabels`, `Activity`, `trace` (`TurnRecord`, `TurnRecorder`, `interpret`), `inspect`
  (context, usage with optional `Pricing` / `ARK_PRICE`, working state, sessions) and
  `ChannelApprover`. Generic commands gain `/rename` and `/project`.
- **`ark-ui`**: the graphical workspace. `ArkUi` builder (accent, logo, welcome, suggestions,
  assets, scripts, stylesheets, handlers), `Host` with a `call` protocol and an event stream,
  a Tauri window (feature `desktop`, default) and a loopback HTTP server with a per-run token.
  The page's extension API (`window.ark`): panels, artifact viewers, tool renderers, actions,
  status lines, events. See `docs/UI.md`.
- Steering: `Steering`, `TurnOptions::steer`, `Harness::steering`. A message sent while the
  agent works reaches the model at the next step; one that arrives too late is returned in
  `HarnessOutcome::steered` for the next turn. `HarnessEvent::Steered`, `Turn::steered`.
- `Usage::cached_tokens` (Anthropic cache reads, OpenAI `cached_tokens`), summed in `Metrics`
  and `SessionUsage`.
- `HarnessEvent::ToolResult::context_tokens` and `ContextReport::retrieved`: what a result
  added to the context, and evidence fetched on demand shown apart from tool results.
- Sessions: `Session::set_title`, `rename`, `transcript_page`; a `turn_traces` store table
  (`put_turn_trace`, `get_turn_trace`), so a stored turn replays as it ran.
- `Ark::shared_engine()`: read-only store access while a turn runs.
- `HarnessEvent` is `Deserialize`.
- Page tests (`crates/ark-ui/tests/page.rs`): the real page in WebKit, opt-in with
  `ARK_UI_PAGE_TESTS=1`. A neutrality test covering the runtime's code as well as the
  front-end layers.

### Breaking
- `ark-cli` renders `ark-interact` and holds no command logic. `ArkCli::new(app)` replaces
  `ArkCli::builder()`; the agent, commands, labels and status move to `App`. `Command::run`
  takes `&mut Interaction` and returns `Reply`; `CommandContext` and `CommandOutcome` are gone.
  `ToolRenderer` is now `ark_interact::ToolLabeler`.
- `ark-cli` keys: while the agent works, enter steers and tab queues (enter used to queue).
- New `HarnessEvent` variant `Steered`; `ToolResult` gains `context_tokens`;
  `HarnessOutcome` gains `steered`. Exhaustive matches need the new arm.
- `Ark` holds its `Engine` in an `Arc`.

### Fixed
- A failed result is no longer reused. Dedup served a failed call's card back as "reused
  prior evidence", so a retry after a timeout or a missing tool never ran again. Only a
  successful result is reused; a failure runs again, and repeating the same failure counts as
  no progress for the stall guard.
- Cancelling a tool call kills its whole process group. Before, a shell's children (for
  example the `sleep` in `sh -c "sleep 30"`) outlived the cancelled call.
- An `artifact` argument given as a short id (`art:dc3b0c93c72c`), a prefix or a name is
  resolved to the full id before the adapter sees it. The model is shown short ids, so its
  calls used to fail with "artifact not found".
- `-p` printed the answer twice after the model narrated before a tool call (the final
  answer was compared with all streamed text, narration included).
- Default activity labels no longer carry domain verbs; "Showing show …" and "1 model calls"
  wording.

## [0.2.0] - 2026-09-30

The interactive shell, and what the runtime needed to support it.

### Added
- **`ark-cli`**: the interactive terminal shell every ARK app opens into. Streaming output,
  semantic activity lines, an approval menu, esc/ctrl+c to stop, input history and multiline
  editing (reedline), sessions and `--resume`, generic slash commands (`/context`, `/usage`,
  `/trace`, `/state`, `/compact`, ...), consumer commands, labels, themes and status sections,
  and a headless `-p` / `--json` / `--events` mode. See `docs/CLI.md`.
- `Provider::complete_streaming` with native streaming for Anthropic, OpenAI and Ollama. The
  default implementation emits the full text once, so every provider works.
- `ApprovalGate`, `Approver`, `ApprovalRequest`, `ApprovalDecision`, `FixedApprover`, and
  `AgentProfile::approval_classifier`: ask a human before `Mutate` actions and remember
  "allow for this session" per approval class.
- `CancelToken` and `Harness::cancel`: cooperative cancellation; `HarnessOutcome::cancelled`.
- `ContextReport`, per-request `HarnessEvent::Context`, and `Harness::preview_context`.
- `Session` (with `CarryPolicy`, `Turn`, `SessionRecord`, `SessionUsage`, `CompactReport`):
  stored conversations with a bounded carry-over, `Session::compact`, and store tables
  `sessions` / `session_turns`. `ResearchState::compact`.
- SDK: `Ark::start_session`, `open_session`, `latest_session`, `ask_session`,
  `context_preview`, `harness`, `router`, `set_router`, `settings`, `find_or_create_project`,
  `ArkBuilder::engine`, `TurnOptions`.
- `ActionDescriptor::reusable`: results that stay valid across turns.
- `ARK_PROVIDER=mock` with `ARK_MOCK_SCRIPT` (and `ARK_MOCK_DELAY_MS`) replays a scripted model
  for end-to-end tests of front-ends. `MockProvider::with_fallback`, `with_chunk_delay`.

### Breaking
- `HarnessEvent` is owned (`String`/`Value` fields instead of borrows), `Clone` and
  `Serialize`, and has new variants: `Step`, `Context`, `TextDelta`, `ModelCall`,
  `ApprovalRequested`, `ApprovalResolved`, `Cancelled`. `ToolCall` gains `id`; `ToolResult`
  gains `id`, `invocation`, `duration_ms`, `error`; `Answer` gains `streamed`. Match arms that
  bind fields need `..`.
- `HarnessOutcome` gains `cancelled`.
- `ArkBuilder` reads `ARK_CTX_BUDGET` for its default budget, as `Harness::new` already did.

### Fixed
- Consumer distillers now run before ARK's common distiller, as `docs/EXTENSION.md` always
  said. Before, a domain `list` or `read` summary was shadowed by the generic one.
- Evidence is only reused across turns for inputs that cannot change (a content-addressed
  `artifact`, or a `reusable` action). A workspace file read in an earlier session is no
  longer served stale. Within a turn, identical reads are still reused. `Mutate` calls are
  never deduplicated.

## [0.1.3] - 2026-09-30

### Added
- `HarnessOutcome::grounded()`: true when the answer cites at least one piece of evidence.
  A convenience for consumers that gate on grounding.

## [0.1.2] - 2026-09-30

### Added
- `ingest_bytes` now also indexes an `ArtifactKind::Other(_)` artifact for full-text search
  when its bytes are valid UTF-8 (previously only `is_textual()` kinds were indexed). Lets a
  consumer's text-bearing domain format (a plist, a JSON trace, an outline) be searchable
  without ARK knowing the format. Binary `Other` bytes fail the UTF-8 guard and are skipped.

### Breaking
- `AcquisitionProvider` is now an **async** trait (`find`/`propose`/`acquire` are `async fn`),
  since acquiring a provider is inherently I/O (probe, install, connect). Refines the seam
  introduced in 0.1.1 before it ships in a released consumer.

## [0.1.1] - 2026-09-30

### Added
- **Capability-acquisition seam** (async `AcquisitionProvider` trait, `AcquisitionResult`), injected
  via `AgentProfile::acquisition`. When a profile supplies one, the runtime enables the
  `find_capability` / `propose_capability` / `acquire_capability` meta-tools and dispatches to
  it (acquisition gated by the same approval as a `Mutate` action). Previously these meta-tools
  were stubbed as "not enabled". Mirrors the existing `KnowledgeProvider` seam and stays
  domain-agnostic (a security agent installs a disassembler; a writing agent could wire a
  thesaurus). Discovered while migrating the security workbench onto ARK.
- **`HarnessEvent::Capability { action, detail }`**: emitted when an acquisition meta-tool
  runs, so a UI can surface it. This is additive; match arms should already carry a `_ =>`.

## [0.1.0] - 2026-09-30

### Added
- Initial extraction of a domain-agnostic agent runtime from a security-research workstation.
- Crates: `ark-core`, `ark-model`, `ark-store`, `ark-tools`, `ark-runtime`, `ark-sdk`.
- Two agent loops (`Agent`, `Harness`) with a managed context budget, model routing,
  deterministic distillation, progressive tool disclosure, a persistent working state +
  investigation graph, a self-check + critic, an approval gate, and full metrics.
- Model providers: Anthropic, OpenAI(-compatible), Ollama, and a scriptable Mock.
- Out-of-process tools via an MCP plugin bridge.
- `AgentProfile` as the single injection point for domain persona, capabilities, distillers,
  and knowledge.
- Example agents: `minimal-agent`, `writing-agent`.
- Docs: README, architecture, extension guide, versioning, roadmap, extraction map.

The security-specific vocabularies of the source (a fixed capability enum, binary/RE artifact
kinds, typed RE/network evidence families, the security persona and distillers) were removed
from the core and made consumer-supplied.
